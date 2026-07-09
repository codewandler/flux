//! Host-terminated PostgreSQL authentication (D-31).
//!
//! The host — not the plugin — speaks the PostgreSQL v3 startup + authentication handshake on an
//! already-dialed socket, so the trusted `sql` plugin never receives the password: it is handed a
//! *post-auth* connection (at the first `ReadyForQuery`) plus the negotiated parameters. This closes
//! the last gap in the references-only invariant for raw-socket, in-band-auth protocols — the one
//! place a plugin still held a secret value (the SCRAM/md5 password).
//!
//! Scope: exactly the auth phase (StartupMessage → Authentication{Ok,cleartext,MD5,SASL
//! SCRAM-SHA-256} → drain to `ReadyForQuery`, capturing `ParameterStatus`/`BackendKeyData`). The
//! Simple Query protocol stays in the plugin, which drives it over the same post-auth `conn_id`. The
//! host speaks **no** SQL; this is not a Postgres client crate.
//!
//! SCRAM-SHA-256 (RFC 5802 / RFC 7677) is implemented in full, including the client-final proof and
//! the **server-signature verification** on the server-final message — a MITM/wrong-server guard: a
//! bad `v=` is rejected rather than trusted.

use std::collections::BTreeMap;
use std::time::Duration;

// hmac 0.13 (digest 0.11): `new_from_slice` moved off `Mac` onto the re-exported `KeyInit`.
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

use flux_system::net::DialStream;

type HmacSha256 = Hmac<Sha256>;

/// Upper bound on the server-supplied SCRAM `i=` iteration count (RFC 5802 §3), enforced *before* any
/// PBKDF2 work. RFC-typical deployments sit in the 4096–100,000 range (Postgres itself defaults to
/// 4096); this ceiling is an order of magnitude above the highest sane value anyone would legitimately
/// configure, yet stops a hostile/MITM'd endpoint from driving `pbkdf2_hmac_sha256`'s `for _ in
/// 1..iterations` loop for `i=2000000000`-style counts, which pegs a CPU core for minutes. The
/// handshake's socket-read `timeout` never covers this — it's pure computation, not I/O.
pub(crate) const MAX_SCRAM_ITERATIONS: u32 = 1_000_000;

/// Connection parameters the host puts in the StartupMessage. All non-secret metadata the plugin
/// already holds (from a discovered endpoint's bare URL or the config DSN) — never the credential.
pub(crate) struct HandshakeParams {
    pub user: String,
    pub database: String,
    pub application_name: String,
}

/// The negotiated connection state the host hands back to the plugin after a successful handshake —
/// `ParameterStatus` values (notably `server_version`) and the backend cancel key. Never the
/// password. The connection is left at the first `ReadyForQuery`, ready for Simple Query.
#[derive(Debug, Default)]
pub(crate) struct HandshakeResult {
    pub parameters: BTreeMap<String, String>,
    pub backend_pid: Option<i32>,
    pub backend_key: Option<i32>,
}

impl HandshakeResult {
    pub fn server_version(&self) -> Option<&str> {
        self.parameters.get("server_version").map(String::as_str)
    }
}

/// Perform the PostgreSQL startup + authentication handshake on `stream`, using `password` (resolved
/// host-side) for whatever auth method the server requests. On success the socket is at the first
/// `ReadyForQuery`; the returned [`HandshakeResult`] carries the negotiated parameters.
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
    };
    hs.startup(params).await?;
    hs.authenticate(&params.user, password).await?;
    hs.drain_to_ready().await
}

/// A minimal blocking-style driver over an async [`DialStream`], holding a small read buffer so a
/// message can be reframed across `conn.read` chunk boundaries.
struct Handshake<'s> {
    stream: &'s mut DialStream,
    timeout: Option<Duration>,
    buf: Vec<u8>,
}

impl Handshake<'_> {
    /// StartupMessage: int32 length, int32 protocol 196608 (3.0), NUL-terminated `key\0value\0`
    /// pairs, final NUL.
    async fn startup(&mut self, params: &HandshakeParams) -> Result<(), String> {
        let mut body: Vec<u8> = Vec::new();
        body.extend_from_slice(&196_608i32.to_be_bytes());
        for (k, v) in [
            ("user", params.user.as_str()),
            ("database", params.database.as_str()),
            ("application_name", params.application_name.as_str()),
            ("client_encoding", "UTF8"),
        ] {
            body.extend_from_slice(k.as_bytes());
            body.push(0);
            body.extend_from_slice(v.as_bytes());
            body.push(0);
        }
        body.push(0);
        let mut msg = Vec::with_capacity(body.len() + 4);
        msg.extend_from_slice(&((body.len() + 4) as i32).to_be_bytes());
        msg.extend_from_slice(&body);
        self.write_all(&msg).await
    }

    /// Drive authentication until `AuthenticationOk`. Handles cleartext, MD5, and SASL SCRAM-SHA-256.
    async fn authenticate(&mut self, user: &str, password: &str) -> Result<(), String> {
        loop {
            let (tag, body) = self.read_message().await?;
            match tag {
                b'R' => {
                    let code = be_i32(&body, 0)?;
                    match code {
                        0 => return Ok(()), // AuthenticationOk
                        3 => {
                            // Cleartext password.
                            self.send_password_message(password.as_bytes()).await?;
                        }
                        5 => {
                            // MD5: salt is the 4 bytes after the code.
                            let salt = body.get(4..8).ok_or("pg: short MD5 salt")?;
                            let token = md5_password(user, password, salt);
                            self.send_password_message(token.as_bytes()).await?;
                        }
                        10 => {
                            // SASL: NUL-separated mechanism list. Require SCRAM-SHA-256.
                            let mechs = parse_cstring_list(&body[4..]);
                            if !mechs.iter().any(|m| m == "SCRAM-SHA-256") {
                                return Err(format!(
                                    "pg: server offered SASL mechanisms {mechs:?}; only SCRAM-SHA-256 is supported"
                                ));
                            }
                            self.scram_authenticate(password).await?;
                        }
                        2 => return Err("pg: KerberosV5 auth unsupported".into()),
                        6 => return Err("pg: SCM credential auth unsupported".into()),
                        7 | 8 => return Err("pg: GSSAPI/SSPI auth unsupported".into()),
                        9 => return Err("pg: SSPI auth unsupported".into()),
                        other => return Err(format!("pg: unsupported auth request {other}")),
                    }
                }
                b'E' => return Err(format!("pg: {}", parse_error(&body))),
                other => {
                    return Err(format!(
                        "pg: unexpected message {:?} during authentication",
                        other as char
                    ))
                }
            }
        }
    }

    /// The full SCRAM-SHA-256 SASL exchange (RFC 5802 / RFC 7677) after the server offered it.
    /// Uses `SCRAM-SHA-256` **without** channel binding — the GS2 header `n,,`.
    async fn scram_authenticate(&mut self, password: &str) -> Result<(), String> {
        let client_nonce = gen_nonce();
        let client_first_bare = format!("n=,r={client_nonce}");
        let client_first = format!("n,,{client_first_bare}");

        // SASLInitialResponse ('p'): mechanism CString + int32 length + the initial-response bytes.
        let mut init: Vec<u8> = Vec::new();
        init.extend_from_slice(b"SCRAM-SHA-256");
        init.push(0);
        init.extend_from_slice(&(client_first.len() as i32).to_be_bytes());
        init.extend_from_slice(client_first.as_bytes());
        self.send_message(b'p', &init).await?;

        // AuthenticationSASLContinue (R, code 11): the server-first message.
        let (tag, body) = self.read_message().await?;
        let server_first = self.expect_sasl(tag, &body, 11, "SASLContinue")?;
        let attrs = parse_scram_attrs(&server_first);
        let combined_nonce = attrs
            .get("r")
            .cloned()
            .ok_or("pg scram: server-first missing nonce")?;
        if !combined_nonce.starts_with(&client_nonce) {
            return Err("pg scram: server nonce does not extend the client nonce".into());
        }
        let salt_b64 = attrs
            .get("s")
            .ok_or("pg scram: server-first missing salt")?;
        let iterations: u32 = attrs
            .get("i")
            .and_then(|i| i.parse().ok())
            .ok_or("pg scram: server-first missing/invalid iteration count")?;
        if iterations > MAX_SCRAM_ITERATIONS {
            return Err(format!(
                "pg scram: server-first iteration count {iterations} exceeds the maximum of \
                 {MAX_SCRAM_ITERATIONS} (refusing to run a hostile/absurd PBKDF2 iteration count)"
            ));
        }
        let salt = base64_decode(salt_b64)?;

        // SaltedPassword = PBKDF2-HMAC-SHA256(password, salt, i).
        let salted = pbkdf2_hmac_sha256(password.as_bytes(), &salt, iterations);
        let client_key = hmac_sha256(&salted, b"Client Key");
        let stored_key = sha256(&client_key);
        let channel_binding = base64_encode(b"n,,"); // GS2 header, no channel binding.
        let client_final_no_proof = format!("c={channel_binding},r={combined_nonce}");
        let auth_message = format!("{client_first_bare},{server_first},{client_final_no_proof}");
        let client_signature = hmac_sha256(&stored_key, auth_message.as_bytes());
        let proof: Vec<u8> = client_key
            .iter()
            .zip(client_signature.iter())
            .map(|(a, b)| a ^ b)
            .collect();
        let client_final = format!("{client_final_no_proof},p={}", base64_encode(&proof));

        // SASLResponse ('p'): the client-final message.
        self.send_message(b'p', client_final.as_bytes()).await?;

        // AuthenticationSASLFinal (R, code 12): verify the server signature (MITM guard).
        let (tag, body) = self.read_message().await?;
        let server_final = self.expect_sasl(tag, &body, 12, "SASLFinal")?;
        let final_attrs = parse_scram_attrs(&server_final);
        let server_sig_b64 = final_attrs
            .get("v")
            .ok_or("pg scram: server-final missing verifier")?;
        let server_key = hmac_sha256(&salted, b"Server Key");
        let expected_sig = hmac_sha256(&server_key, auth_message.as_bytes());
        if base64_decode(server_sig_b64)? != expected_sig {
            return Err("pg scram: server signature verification failed".into());
        }
        // The following AuthenticationOk is consumed by the authenticate() loop.
        Ok(())
    }

    /// Expect a SASL Authentication message (`R`) with the given sub-code, returning its UTF-8 payload.
    fn expect_sasl(
        &self,
        tag: u8,
        body: &[u8],
        want_code: i32,
        what: &str,
    ) -> Result<String, String> {
        if tag == b'E' {
            return Err(format!("pg: {}", parse_error(body)));
        }
        if tag != b'R' {
            return Err(format!(
                "pg scram: expected Authentication ({what}), got {:?}",
                tag as char
            ));
        }
        let code = be_i32(body, 0)?;
        if code != want_code {
            return Err(format!(
                "pg scram: expected auth code {want_code} ({what}), got {code}"
            ));
        }
        Ok(String::from_utf8_lossy(&body[4..]).into_owned())
    }

    /// Send a PasswordMessage ('p') with a NUL-terminated payload (cleartext/MD5 path).
    async fn send_password_message(&mut self, payload: &[u8]) -> Result<(), String> {
        let mut body = payload.to_vec();
        body.push(0);
        self.send_message(b'p', &body).await
    }

    /// Drain messages until the first `ReadyForQuery`, capturing `ParameterStatus` +
    /// `BackendKeyData` into the returned [`HandshakeResult`].
    async fn drain_to_ready(&mut self) -> Result<HandshakeResult, String> {
        let mut result = HandshakeResult::default();
        loop {
            let (tag, body) = self.read_message().await?;
            match tag {
                b'Z' => return Ok(result), // ReadyForQuery
                b'S' => {
                    // ParameterStatus: name\0value\0.
                    let parts = parse_cstring_list(&body);
                    if parts.len() >= 2 {
                        result.parameters.insert(parts[0].clone(), parts[1].clone());
                    }
                }
                b'K' => {
                    // BackendKeyData: int32 pid, int32 secret key.
                    result.backend_pid = be_i32(&body, 0).ok();
                    result.backend_key = be_i32(&body, 4).ok();
                }
                b'E' => return Err(format!("pg: {}", parse_error(&body))),
                b'N' => {} // NoticeResponse — ignored.
                _ => {}    // Other startup messages — ignored.
            }
        }
    }

    // --- framing over the async DialStream ---

    /// Send a tagged message: 1 byte tag, int32 length (incl. itself), then `body`.
    async fn send_message(&mut self, tag: u8, body: &[u8]) -> Result<(), String> {
        let mut msg = Vec::with_capacity(body.len() + 5);
        msg.push(tag);
        msg.extend_from_slice(&((body.len() + 4) as i32).to_be_bytes());
        msg.extend_from_slice(body);
        self.write_all(&msg).await
    }

    /// Read one tagged backend message: 1 byte tag + int32 length, then `length-4` body bytes.
    async fn read_message(&mut self) -> Result<(u8, Vec<u8>), String> {
        let header = self.read_exact(5).await?;
        let tag = header[0];
        let len = i32::from_be_bytes([header[1], header[2], header[3], header[4]]);
        if len < 4 {
            return Err(format!("pg: invalid message length {len}"));
        }
        let body_len = (len - 4) as usize;
        let body = if body_len > 0 {
            self.read_exact(body_len).await?
        } else {
            Vec::new()
        };
        Ok((tag, body))
    }

    async fn write_all(&mut self, data: &[u8]) -> Result<(), String> {
        self.stream
            .write_all(data)
            .await
            .map_err(|e| format!("pg: write failed: {e}"))
    }

    /// Read exactly `n` bytes, buffering across `conn.read` chunks; EOF mid-read is an error.
    async fn read_exact(&mut self, n: usize) -> Result<Vec<u8>, String> {
        while self.buf.len() < n {
            let want = n - self.buf.len();
            let chunk = match self.timeout {
                Some(dur) => tokio::time::timeout(dur, self.stream.read(want))
                    .await
                    .map_err(|_| "pg: handshake read timed out".to_string())?
                    .map_err(|e| format!("pg: read failed: {e}"))?,
                None => self
                    .stream
                    .read(want)
                    .await
                    .map_err(|e| format!("pg: read failed: {e}"))?,
            };
            if chunk.is_empty() {
                return Err("pg: connection closed mid-message (EOF)".into());
            }
            self.buf.extend_from_slice(&chunk);
        }
        let rest = self.buf.split_off(n);
        Ok(std::mem::replace(&mut self.buf, rest))
    }
}

// ===========================================================================
// Wire-frame parsing
// ===========================================================================

fn be_i32(buf: &[u8], at: usize) -> Result<i32, String> {
    buf.get(at..at + 4)
        .map(|b| i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or_else(|| "pg: truncated int32".into())
}

/// Parse ErrorResponse ('E') into a human message (the 'M' field, with the 'S'/'C' prefix when present).
fn parse_error(body: &[u8]) -> String {
    let mut severity = String::new();
    let mut code = String::new();
    let mut message = String::new();
    let mut i = 0;
    while i < body.len() && body[i] != 0 {
        let field = body[i];
        i += 1;
        let start = i;
        while i < body.len() && body[i] != 0 {
            i += 1;
        }
        let value = String::from_utf8_lossy(&body[start..i]).into_owned();
        i += 1; // NUL
        match field {
            b'S' => severity = value,
            b'C' => code = value,
            b'M' => message = value,
            _ => {}
        }
    }
    match (severity.is_empty(), code.is_empty()) {
        (false, false) => format!("{severity} {code}: {message}"),
        _ => message,
    }
}

/// Split a buffer of NUL-terminated C strings into a list (a trailing empty terminator is dropped).
fn parse_cstring_list(buf: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, &b) in buf.iter().enumerate() {
        if b == 0 {
            if i > start {
                out.push(String::from_utf8_lossy(&buf[start..i]).into_owned());
            }
            start = i + 1;
        }
    }
    out
}

/// Parse `key=value,key=value` SCRAM attributes (values may contain `=`).
fn parse_scram_attrs(s: &str) -> std::collections::HashMap<String, String> {
    s.split(',')
        .filter_map(|part| part.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

// ===========================================================================
// Crypto primitives (SCRAM / MD5)
// ===========================================================================

pub(crate) fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

pub(crate) fn sha256(data: &[u8]) -> Vec<u8> {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().to_vec()
}

/// PBKDF2-HMAC-SHA256 with a single 32-byte output block (SCRAM uses dkLen = hashLen, so block 1 only).
pub(crate) fn pbkdf2_hmac_sha256(password: &[u8], salt: &[u8], iterations: u32) -> Vec<u8> {
    // U1 = HMAC(password, salt || INT(1)); Ui = HMAC(password, Ui-1); result = U1 ^ U2 ^ … ^ Uc.
    let mut salted = salt.to_vec();
    salted.extend_from_slice(&1u32.to_be_bytes());
    let mut u = hmac_sha256(password, &salted);
    let mut result = u.clone();
    for _ in 1..iterations {
        u = hmac_sha256(password, &u);
        for (r, x) in result.iter_mut().zip(u.iter()) {
            *r ^= *x;
        }
    }
    result
}

/// `md5` PostgreSQL password token: `"md5" + md5_hex(md5_hex(password+user) + salt)`.
fn md5_password(user: &str, password: &str, salt: &[u8]) -> String {
    let inner = md5_hex(format!("{password}{user}").as_bytes());
    let mut outer_input = inner.into_bytes();
    outer_input.extend_from_slice(salt);
    format!("md5{}", md5_hex(&outer_input))
}

/// A small, self-contained MD5 (RFC 1321) — used only for the legacy MD5 auth token. Postgres MD5 is
/// not a security boundary (the server picks it), so a vendored MD5 here avoids another dependency.
pub(crate) fn md5_hex(input: &[u8]) -> String {
    let digest = md5_digest(input);
    let mut s = String::with_capacity(32);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn md5_digest(input: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];
    let mut a0: u32 = 0x67452301;
    let mut b0: u32 = 0xefcdab89;
    let mut c0: u32 = 0x98badcfe;
    let mut d0: u32 = 0x10325476;

    let mut msg = input.to_vec();
    let bit_len = (input.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());

    for chunk in msg.chunks_exact(64) {
        let mut m = [0u32; 16];
        for (i, word) in m.iter_mut().enumerate() {
            *word = u32::from_le_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((b & c) | (!b & d), i),
                16..=31 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(K[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&a0.to_le_bytes());
    out[4..8].copy_from_slice(&b0.to_le_bytes());
    out[8..12].copy_from_slice(&c0.to_le_bytes());
    out[12..16].copy_from_slice(&d0.to_le_bytes());
    out
}

/// A 24-char alphanumeric client nonce (SCRAM forbids `,` and `=`; alphanumerics are always safe).
fn gen_nonce() -> String {
    use rand::RngExt;
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    // rand 0.10: `thread_rng()`/`Rng::gen_range` became `rng()`/`RngExt::random_range`
    // (same generator underneath).
    let mut rng = rand::rng();
    (0..24)
        .map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char)
        .collect()
}

pub(crate) fn base64_encode(data: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(data)
}

pub(crate) fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .map_err(|e| format!("pg scram: bad base64: {e}"))
}

// ===========================================================================
// Protocol seam
// ===========================================================================

/// Dispatch a host-terminated handshake by protocol. **Postgres is implemented**; mysql (handshake
/// v10) and Asterisk AMI (line-based login) are the declared follow-ons — add an arm here and a
/// sibling module beside `pg`. The `credential` capability stays available (gated) for any protocol
/// not yet host-terminated, so those plugins are unaffected until their terminator lands.
pub(crate) async fn terminate_handshake(
    protocol: &str,
    stream: &mut DialStream,
    params: &HandshakeParams,
    password: &str,
    timeout: Option<Duration>,
) -> Result<HandshakeResult, String> {
    match protocol.trim().to_ascii_lowercase().as_str() {
        "postgres" | "postgresql" | "pg" | "pgx" => {
            authenticate(stream, params, password, timeout).await
        }
        other => Err(format!(
            "conn.authenticate: host-terminated auth is not implemented for protocol {other:?} \
             (postgres only for now — mysql/AMI are follow-ons); the gated `credential` capability \
             remains for those"
        )),
    }
}
