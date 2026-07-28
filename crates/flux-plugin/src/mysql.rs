//! Host-terminated MySQL / MariaDB authentication (D-196).
//!
//! The MySQL half of what [`crate::pg`] does for Postgres: the host speaks Handshake v10 →
//! HandshakeResponse41 → OK on an already-dialed socket, so the `sql` plugin is handed a *post-auth*
//! connection and never receives the password. Closes the `mysql` half of the residual D-31 recorded
//! when it terminated the Postgres handshake.
//!
//! ## Why this exists instead of a driver crate
//! A MySQL connection is a TCP connection, and `ConnStream` is already `std::io::Read + Write`, so
//! the obvious move is to hand that stream to `mysql_async` the way Go injects a `net.Conn` via
//! `RegisterDialContext`. It does not work, for a reason that outlives the ecosystem gap: a driver
//! crate insists on performing its **own** handshake, which requires the password inside the plugin
//! — exactly what the reference invariant forbids. Once the host terminates auth, no driver crate is
//! usable anyway, because none will resume from a mid-stream post-auth state. See
//! `docs/designs/mariadb-support.md`.
//!
//! ## Scope
//! `mysql_native_password` only. It is MariaDB's default when a user is created without an explicit
//! plugin, is statically linked into the server, and covers MySQL 5.7 as well. It is *simpler* than
//! the SCRAM-SHA-256 already shipped for Postgres: no PBKDF2, no iteration ceiling, and no
//! server-signature verification, because the protocol offers the client no way to authenticate the
//! server. `caching_sha2_password` (MySQL 8.0+ default), `ed25519`, and `parsec` each fail with a
//! distinct error naming the plugin — including when requested mid-handshake via AuthSwitchRequest.
//!
//! ## Honesty note
//! Covered by hermetic tests against a scripted MySQL-server stub — bytes a test author wrote. First
//! contact with a real MariaDB server is unverified, the same standing caveat the Postgres path
//! carries.

use std::time::Duration;

use flux_system::net::DialStream;

use crate::handshake::{HandshakeParams, HandshakeResult, MAX_MESSAGE_BYTES};

// --- capability flags (only the ones we set or read) ---

const CLIENT_LONG_PASSWORD: u32 = 0x0000_0001;
const CLIENT_LONG_FLAG: u32 = 0x0000_0004;
const CLIENT_CONNECT_WITH_DB: u32 = 0x0000_0008;
const CLIENT_LOCAL_FILES: u32 = 0x0000_0080;
const CLIENT_PROTOCOL_41: u32 = 0x0000_0200;
const CLIENT_TRANSACTIONS: u32 = 0x0000_2000;
const CLIENT_SECURE_CONNECTION: u32 = 0x0000_8000;
const CLIENT_PLUGIN_AUTH: u32 = 0x0008_0000;
const CLIENT_DEPRECATE_EOF: u32 = 0x0100_0000;

/// The auth plugin this module implements.
const NATIVE_PASSWORD: &str = "mysql_native_password";

/// The nonce length `mysql_native_password` requires (8-byte part 1 + 12-byte part 2). Enforced so a
/// truncated or absent scramble can never produce a nonce-free token.
const NATIVE_SCRAMBLE_LEN: usize = 20;

/// Maximum payload we announce per packet (the protocol's own 3-byte ceiling, 16 MiB − 1).
const MAX_PACKET_SIZE: u32 = 0x00FF_FFFF;

/// `utf8mb4_general_ci`. Matches the `client_encoding=UTF8` the Postgres path sets.
const CHARSET_UTF8MB4: u8 = 45;

/// Perform the MySQL/MariaDB connection handshake on `stream`, using `password` (resolved
/// host-side). On success the socket sits immediately after the auth OK packet, ready for the
/// plugin's `COM_QUERY` traffic; the returned [`HandshakeResult`] carries the server version and the
/// negotiated capability flags.
pub(crate) async fn authenticate(
    stream: &mut DialStream,
    params: &HandshakeParams,
    password: &str,
    timeout: Option<Duration>,
) -> Result<HandshakeResult, String> {
    let mut hs = Handshake {
        stream,
        timeout,
        buf: Vec::new(),
        seq: 0,
    };
    let greeting = hs.read_greeting().await?;
    let capabilities = hs.send_response(params, password, &greeting).await?;
    hs.finish_auth(password, &greeting).await?;

    let mut result = HandshakeResult {
        capabilities: Some(capabilities),
        ..Default::default()
    };
    result
        .parameters
        .insert("server_version".into(), greeting.server_version.clone());
    result.backend_pid = Some(greeting.connection_id as i32);
    Ok(result)
}

/// The parsed Handshake v10 greeting.
struct Greeting {
    server_version: String,
    connection_id: u32,
    capabilities: u32,
    /// The 20-byte `auth-plugin-data` scramble, reassembled from its two wire fields.
    scramble: Vec<u8>,
    auth_plugin: String,
}

/// A blocking-style driver over an async [`DialStream`], holding a read buffer so a packet can be
/// reframed across `conn.read` chunk boundaries, plus the packet sequence id the protocol requires
/// the client to echo and increment.
struct Handshake<'s> {
    stream: &'s mut DialStream,
    timeout: Option<Duration>,
    buf: Vec<u8>,
    seq: u8,
}

impl Handshake<'_> {
    /// Read and parse Handshake v10.
    ///
    /// Layout: `0x0a`, NUL-terminated server version, 4-byte connection id, 8 scramble bytes, filler,
    /// 2 low capability bytes, charset, 2 status bytes, 2 high capability bytes, scramble length,
    /// 10 reserved bytes, the rest of the scramble, then the auth plugin name.
    async fn read_greeting(&mut self) -> Result<Greeting, String> {
        let payload = self.read_packet().await?;
        let mut r = Reader::new(&payload);

        let protocol_version = r.u8("protocol version")?;
        if protocol_version == 0xff {
            return Err(format!("mysql: {}", parse_err_packet(&payload[1..])));
        }
        if protocol_version != 10 {
            return Err(format!(
                "mysql: unsupported protocol version {protocol_version} (only the v10 handshake is \
                 implemented; v9 predates MySQL 4.1)"
            ));
        }

        let server_version = r.nul_string("server version")?;
        let connection_id = r.u32("connection id")?;
        let mut scramble = r.bytes(8, "scramble part 1")?.to_vec();
        r.skip(1, "filler")?;
        let cap_low = r.u16("capability flags (low)")? as u32;

        // A pre-4.1 server stops here. Everything past this point is optional on the wire — and the
        // block below reads exactly 16 bytes (charset 1 + status 2 + capability high 2 + auth-data
        // length 1 + reserved 10), so anything less must fall through to the pre-4.1 diagnosis
        // rather than die mid-read.
        let (capabilities, scramble_len) = if r.remaining() >= 16 {
            r.skip(1, "character set")?;
            r.skip(2, "status flags")?;
            let cap_high = r.u16("capability flags (high)")? as u32;
            let scramble_len = r.u8("auth plugin data length")?;
            r.skip(10, "reserved")?;
            (cap_low | (cap_high << 16), scramble_len)
        } else {
            (cap_low, 0)
        };

        if capabilities & CLIENT_PROTOCOL_41 == 0 {
            return Err(
                "mysql: server does not support the 4.1 protocol (CLIENT_PROTOCOL_41 unset); \
                 pre-4.1 authentication is not implemented"
                    .into(),
            );
        }
        // `send_response` frames the auth response with a 1-byte length prefix, which is the
        // CLIENT_SECURE_CONNECTION encoding; the pre-secure-connection layout wants a NUL-terminated
        // response instead. Rather than carry an untestable legacy branch, require the flag — every
        // 4.1+ server sets it — so the encoding assumption is enforced instead of merely assumed.
        if capabilities & CLIENT_SECURE_CONNECTION == 0 {
            return Err(
                "mysql: server does not advertise CLIENT_SECURE_CONNECTION; the pre-4.1 password \
                 encoding is not implemented"
                    .into(),
            );
        }

        // Part 2 is `max(13, scramble_len - 8)` bytes with a trailing NUL that is not part of the
        // scramble. Servers are inconsistent about the declared length, so trust the larger of the
        // declared size and the documented 13-byte floor, then drop the NUL.
        if capabilities & CLIENT_SECURE_CONNECTION != 0 {
            let part2_len = std::cmp::max(13, scramble_len.saturating_sub(8) as usize);
            let part2 = r.bytes(part2_len.min(r.remaining()), "scramble part 2")?;
            let part2 = part2.strip_suffix(&[0]).unwrap_or(part2);
            scramble.extend_from_slice(part2);
        }

        // Some servers omit the trailing NUL on this, the last field. A server that does not
        // advertise CLIENT_PLUGIN_AUTH implies native password.
        let auth_plugin = if capabilities & CLIENT_PLUGIN_AUTH != 0 && r.remaining() > 0 {
            r.nul_string_or_rest("auth plugin name")?
        } else {
            NATIVE_PASSWORD.to_string()
        };

        Ok(Greeting {
            server_version,
            connection_id,
            capabilities,
            scramble,
            auth_plugin,
        })
    }

    /// Send HandshakeResponse41 and return the capability flags we negotiated (ours ∩ the server's).
    ///
    /// Layout: 4-byte capabilities, 4-byte max packet size, charset, 23 reserved zero bytes,
    /// NUL-terminated username, length-prefixed auth response, optional NUL-terminated database,
    /// optional NUL-terminated auth plugin name.
    async fn send_response(
        &mut self,
        params: &HandshakeParams,
        password: &str,
        greeting: &Greeting,
    ) -> Result<u32, String> {
        // Never announce CLIENT_LOCAL_FILES: it lets a malicious server ask the client to read a
        // local file (the `LOAD DATA LOCAL INFILE` attack). We are a host process with filesystem
        // access the plugin deliberately lacks, so the bit stays off regardless of server support.
        let mut want = CLIENT_LONG_PASSWORD
            | CLIENT_LONG_FLAG
            | CLIENT_PROTOCOL_41
            | CLIENT_TRANSACTIONS
            | CLIENT_SECURE_CONNECTION
            | CLIENT_PLUGIN_AUTH
            | CLIENT_DEPRECATE_EOF;
        if !params.database.is_empty() {
            want |= CLIENT_CONNECT_WITH_DB;
        }
        let negotiated = (want & greeting.capabilities) & !CLIENT_LOCAL_FILES;

        let auth_response = auth_response_for(&greeting.auth_plugin, password, &greeting.scramble)?;

        let mut body: Vec<u8> = Vec::new();
        body.extend_from_slice(&negotiated.to_le_bytes());
        body.extend_from_slice(&MAX_PACKET_SIZE.to_le_bytes());
        body.push(CHARSET_UTF8MB4);
        body.extend_from_slice(&[0u8; 23]);
        body.extend_from_slice(params.user.as_bytes());
        body.push(0);

        // With CLIENT_SECURE_CONNECTION the response is 1-byte length-prefixed.
        body.push(auth_response.len() as u8);
        body.extend_from_slice(&auth_response);

        if negotiated & CLIENT_CONNECT_WITH_DB != 0 {
            body.extend_from_slice(params.database.as_bytes());
            body.push(0);
        }
        if negotiated & CLIENT_PLUGIN_AUTH != 0 {
            body.extend_from_slice(greeting.auth_plugin.as_bytes());
            body.push(0);
        }

        self.write_packet(&body).await?;
        Ok(negotiated)
    }

    /// Read the server's verdict, handling one AuthSwitchRequest. Returns once auth is OK.
    async fn finish_auth(&mut self, password: &str, greeting: &Greeting) -> Result<(), String> {
        let payload = self.read_packet().await?;
        match payload.first() {
            Some(0x00) => Ok(()), // OK packet — authenticated.
            Some(0xff) => Err(format!("mysql: {}", parse_err_packet(&payload[1..]))),
            Some(0xfe) => {
                // AuthSwitchRequest: NUL-terminated plugin name, then a fresh scramble.
                let mut r = Reader::new(&payload[1..]);
                let plugin = r.nul_string_or_rest("auth switch plugin name")?;
                let scramble = r.rest();
                let scramble = scramble.strip_suffix(&[0]).unwrap_or(scramble);
                let response = auth_response_for(&plugin, password, scramble)?;
                self.write_packet(&response).await?;
                let verdict = self.read_packet().await?;
                match verdict.first() {
                    Some(0x00) => Ok(()),
                    Some(0xff) => Err(format!("mysql: {}", parse_err_packet(&verdict[1..]))),
                    other => Err(format!(
                        "mysql: unexpected packet {other:?} after auth-switch response"
                    )),
                }
            }
            Some(0x01) => Err(format!(
                "mysql: server requested additional auth data for plugin {:?}, which is not \
                 implemented (only {NATIVE_PASSWORD} is)",
                greeting.auth_plugin
            )),
            other => Err(format!(
                "mysql: unexpected packet header {other:?} after HandshakeResponse41"
            )),
        }
    }

    // --- framing over the async DialStream ---

    /// Write one packet: 3-byte little-endian payload length, 1-byte sequence id, payload.
    async fn write_packet(&mut self, payload: &[u8]) -> Result<(), String> {
        let len = payload.len();
        if len > MAX_PACKET_SIZE as usize {
            return Err(format!(
                "mysql: outgoing payload of {len} bytes exceeds one packet; auth never needs this"
            ));
        }
        let mut msg = Vec::with_capacity(len + 4);
        msg.extend_from_slice(&(len as u32).to_le_bytes()[..3]);
        msg.push(self.seq);
        msg.extend_from_slice(payload);
        self.seq = self.seq.wrapping_add(1);
        self.stream
            .write_all(&msg)
            .await
            .map_err(|e| format!("mysql: write failed: {e}"))
    }

    /// Read one packet's payload, reassembling a `0xFFFFFF`-length continuation chain, and adopt the
    /// server's sequence id so our next write continues it.
    async fn read_packet(&mut self) -> Result<Vec<u8>, String> {
        let mut payload = Vec::new();
        loop {
            let header = self.read_exact(4).await?;
            let len = u32::from_le_bytes([header[0], header[1], header[2], 0]) as usize;
            self.seq = header[3].wrapping_add(1);
            if payload.len() + len > MAX_MESSAGE_BYTES {
                return Err(format!(
                    "mysql: server-declared payload of {} bytes exceeds the {MAX_MESSAGE_BYTES}-byte \
                     cap (refusing to buffer a hostile/absurd frame)",
                    payload.len() + len
                ));
            }
            if len > 0 {
                payload.extend_from_slice(&self.read_exact(len).await?);
            }
            // A packet at exactly the 3-byte maximum continues into the next one.
            if len < MAX_PACKET_SIZE as usize {
                break;
            }
        }
        Ok(payload)
    }

    /// Read exactly `n` bytes, buffering across `conn.read` chunks; EOF mid-read is an error.
    async fn read_exact(&mut self, n: usize) -> Result<Vec<u8>, String> {
        while self.buf.len() < n {
            let want = n - self.buf.len();
            let chunk = match self.timeout {
                Some(dur) => tokio::time::timeout(dur, self.stream.read(want))
                    .await
                    .map_err(|_| "mysql: handshake read timed out".to_string())?
                    .map_err(|e| format!("mysql: read failed: {e}"))?,
                None => self
                    .stream
                    .read(want)
                    .await
                    .map_err(|e| format!("mysql: read failed: {e}"))?,
            };
            if chunk.is_empty() {
                return Err("mysql: connection closed mid-packet (EOF)".into());
            }
            self.buf.extend_from_slice(&chunk);
        }
        Ok(self.buf.drain(..n).collect())
    }
}

/// Compute the auth response for `plugin`, or refuse with an error naming it.
pub(crate) fn auth_response_for(
    plugin: &str,
    password: &str,
    scramble: &[u8],
) -> Result<Vec<u8>, String> {
    match plugin {
        NATIVE_PASSWORD => {
            // Refuse a short/absent scramble. The pre-4.1 "old auth switch request" is a bare `0xfe`
            // packet carrying neither a plugin name nor a nonce; answering it with
            // `native_password(pw, &[])` would emit a token derived from the password ALONE, with no
            // server nonce — destroying exactly the replay resistance this scheme rests on. A
            // hostile server could harvest that token by asking for a downgrade.
            if scramble.len() < NATIVE_SCRAMBLE_LEN {
                return Err(format!(
                    "mysql: server supplied a {}-byte auth scramble, but {NATIVE_PASSWORD} requires \
                     {NATIVE_SCRAMBLE_LEN}; refusing to send a nonce-free password token (this is \
                     what a pre-4.1 auth downgrade looks like)",
                    scramble.len()
                ));
            }
            Ok(native_password(password, scramble))
        }
        "" => Err(
            "mysql: server requested an unnamed auth plugin (a pre-4.1 auth-switch downgrade); \
             refusing rather than guessing a scheme"
                .into(),
        ),
        "caching_sha2_password" => Err(
            "mysql: server requested `caching_sha2_password` (the MySQL 8.0+ default), which is not \
             yet implemented — its full-auth path needs an RSA public-key exchange or an existing \
             TLS channel. Either create the user with `IDENTIFIED WITH mysql_native_password`, or \
             use MariaDB, whose default this is not."
                .into(),
        ),
        "ed25519" => Err(
            "mysql: server requested MariaDB's `ed25519` auth plugin, which is not yet implemented. \
             Create the user with `IDENTIFIED VIA mysql_native_password` to connect."
                .into(),
        ),
        "parsec" => Err(
            "mysql: server requested MariaDB's `parsec` auth plugin, which is not yet implemented. \
             Create the user with `IDENTIFIED VIA mysql_native_password` to connect."
                .into(),
        ),
        "mysql_clear_password" => Err(
            "mysql: server requested `mysql_clear_password`, which sends the password in the clear; \
             refusing on an unencrypted socket."
                .into(),
        ),
        other => Err(format!(
            "mysql: server requested auth plugin {other:?}, which is not implemented (only \
             {NATIVE_PASSWORD} is)"
        )),
    }
}

/// `mysql_native_password`: `SHA1(pw) XOR SHA1(scramble ‖ SHA1(SHA1(pw)))`.
///
/// An empty password sends an empty response — that is the protocol's own convention, not a
/// shortcut.
pub(crate) fn native_password(password: &str, scramble: &[u8]) -> Vec<u8> {
    if password.is_empty() {
        return Vec::new();
    }
    let stage1 = sha1(password.as_bytes());
    let stage2 = sha1(&stage1);
    let mut seed = scramble.to_vec();
    seed.extend_from_slice(&stage2);
    let stage3 = sha1(&seed);
    stage1
        .iter()
        .zip(stage3.iter())
        .map(|(a, b)| a ^ b)
        .collect()
}

/// Parse an ERR packet body (after the `0xff` header): 2-byte error code, then — with
/// CLIENT_PROTOCOL_41 — a `#` marker and a 5-byte SQLSTATE, then the message.
fn parse_err_packet(body: &[u8]) -> String {
    if body.len() < 2 {
        return "server error (truncated ERR packet)".into();
    }
    let code = u16::from_le_bytes([body[0], body[1]]);
    let rest = &body[2..];
    let (sqlstate, message) = if rest.first() == Some(&b'#') && rest.len() >= 6 {
        (
            Some(String::from_utf8_lossy(&rest[1..6]).into_owned()),
            String::from_utf8_lossy(&rest[6..]).into_owned(),
        )
    } else {
        (None, String::from_utf8_lossy(rest).into_owned())
    };
    match sqlstate {
        Some(state) => format!("server error {code} ({state}): {message}"),
        None => format!("server error {code}: {message}"),
    }
}

/// A small, self-contained SHA-1 (RFC 3174) — used only for the `mysql_native_password` token.
///
/// Vendored for the same reason `pg.rs` vendors MD5: the *server* picks this algorithm, so it is not
/// a security boundary we are choosing, and a ~50-line implementation avoids adding a dependency to
/// the published crate closure for one legacy auth token.
pub(crate) fn sha1(input: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];

    let mut msg = input.to_vec();
    let bit_len = (input.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for block in msg.chunks_exact(64) {
        let mut w = [0u32; 80];
        for (i, word) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }

        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }

    let mut out = [0u8; 20];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// A bounds-checked cursor over a packet payload. Every accessor names the field it is reading so a
/// truncated/hostile packet produces a diagnosable error instead of a panic.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    fn need(&self, n: usize, what: &str) -> Result<(), String> {
        if self.remaining() < n {
            return Err(format!(
                "mysql: packet truncated reading {what} (need {n} bytes, {} left)",
                self.remaining()
            ));
        }
        Ok(())
    }

    fn u8(&mut self, what: &str) -> Result<u8, String> {
        self.need(1, what)?;
        let v = self.data[self.pos];
        self.pos += 1;
        Ok(v)
    }

    fn u16(&mut self, what: &str) -> Result<u16, String> {
        self.need(2, what)?;
        let v = u16::from_le_bytes([self.data[self.pos], self.data[self.pos + 1]]);
        self.pos += 2;
        Ok(v)
    }

    fn u32(&mut self, what: &str) -> Result<u32, String> {
        self.need(4, what)?;
        let v = u32::from_le_bytes([
            self.data[self.pos],
            self.data[self.pos + 1],
            self.data[self.pos + 2],
            self.data[self.pos + 3],
        ]);
        self.pos += 4;
        Ok(v)
    }

    fn bytes(&mut self, n: usize, what: &str) -> Result<&'a [u8], String> {
        self.need(n, what)?;
        let v = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(v)
    }

    fn skip(&mut self, n: usize, what: &str) -> Result<(), String> {
        self.need(n, what)?;
        self.pos += n;
        Ok(())
    }

    fn nul_string(&mut self, what: &str) -> Result<String, String> {
        let end = self.data[self.pos..]
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| format!("mysql: packet truncated reading {what} (no NUL terminator)"))?;
        let s = String::from_utf8_lossy(&self.data[self.pos..self.pos + end]).into_owned();
        self.pos += end + 1;
        Ok(s)
    }

    /// A NUL-terminated string, tolerating a server that omits the terminator on the final field.
    fn nul_string_or_rest(&mut self, what: &str) -> Result<String, String> {
        match self.data[self.pos..].iter().position(|&b| b == 0) {
            Some(_) => self.nul_string(what),
            None => Ok(String::from_utf8_lossy(self.rest()).into_owned()),
        }
    }

    fn rest(&mut self) -> &'a [u8] {
        let v = &self.data[self.pos..];
        self.pos = self.data.len();
        v
    }
}
