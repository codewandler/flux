//! Network egress guard — the single SSRF policy reused by every surface that fetches a URL
//! (`flux-capabilities`' `browser` web_fetch, `flux-plugin`'s `http.do`).
//!
//! Beyond rejecting non-HTTP schemes and raw private/loopback IP literals, this **resolves the
//! host to IP addresses** and blocks the request if any resolved address is private, loopback,
//! link-local, unique-local, CGNAT, or an IPv4-mapped form of those — so a hostname pointing at
//! `169.254.169.254` (cloud metadata) or `[::ffff:10.0.0.1]` can't slip through. DNS rebinding
//! (a different answer at connect time) is still possible; this is defense-in-depth, not a
//! complete TOCTOU fix.

use std::net::{IpAddr, Ipv4Addr, ToSocketAddrs};

use flux_core::{Error, Result};

/// Reject URLs that aren't safe to fetch. With `allow_private`, only the scheme check applies.
pub fn guard_url(raw: &str, allow_private: bool) -> Result<url::Url> {
    let url = url::Url::parse(raw).map_err(|e| Error::Other(format!("invalid url: {e}")))?;
    match url.scheme() {
        "http" | "https" => {}
        other => return Err(Error::Other(format!("unsupported url scheme: {other}"))),
    }
    if allow_private {
        return Ok(url);
    }
    // `Host` parses literal IPs into typed addresses (so an IPv6 literal isn't a bracketed string).
    match url.host() {
        None => Err(Error::Other("url has no host".into())),
        Some(url::Host::Ipv4(v4)) => block_if(IpAddr::V4(v4), &v4.to_string()).map(|()| url),
        Some(url::Host::Ipv6(v6)) => block_if(IpAddr::V6(v6), &v6.to_string()).map(|()| url),
        Some(url::Host::Domain(domain)) => {
            // Block internal hostnames outright (these often front link-local metadata services).
            let lower = domain.to_ascii_lowercase();
            if lower == "localhost"
                || lower.ends_with(".localhost")
                || lower == "metadata.google.internal"
                || lower.ends_with(".internal")
            {
                return Err(Error::Other(format!(
                    "refusing to fetch internal host {domain}"
                )));
            }
            // Resolve to IPs and reject if ANY resolved address is in a blocked range. An
            // unresolvable host is left to fail at connect time (it's not an SSRF).
            let port = url.port_or_known_default().unwrap_or(80);
            if let Ok(addrs) = (domain, port).to_socket_addrs() {
                for sa in addrs {
                    block_if(sa.ip(), domain)?;
                }
            }
            Ok(url)
        }
    }
}

/// `Err` if `ip` is in a range the agent may never reach (SSRF protection); `Ok(())` otherwise.
fn block_if(ip: IpAddr, host: &str) -> Result<()> {
    if is_blocked_ip(ip) {
        return Err(Error::Other(format!(
            "refusing to fetch private/loopback/link-local address {ip} ({host})"
        )));
    }
    Ok(())
}

/// Whether an IP is in a range we never let the agent reach (SSRF protection).
fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_blocked_v4(v4),
        IpAddr::V6(v6) => {
            // An IPv4-mapped IPv6 address (`::ffff:a.b.c.d`) is just IPv4 — check it as such.
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_blocked_v4(mapped);
            }
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
        }
    }
}

fn is_blocked_v4(v4: Ipv4Addr) -> bool {
    let o = v4.octets();
    v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local() // 169.254/16 — includes the 169.254.169.254 metadata endpoint
        || v4.is_unspecified()
        || v4.is_broadcast()
        || v4.is_documentation()
        || o[0] == 0 // 0.0.0.0/8 "this network"
        || (o[0] == 100 && (64..=127).contains(&o[1])) // 100.64/10 CGNAT
}

// ---------------------------------------------------------------------------
// Guarded socket dialer — the raw-connection equivalent of `guard_url`, for plugins whose backend
// speaks a wire protocol over a TCP or Unix socket rather than HTTP (sql, docker, asterisk). The
// same SSRF egress policy applies to TCP; Unix sockets are local and gated by capability, not here.
// Story D-12 (plugin protocol parity).
// ---------------------------------------------------------------------------

use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A socket target to dial.
#[derive(Debug, Clone)]
pub enum DialTarget {
    /// A TCP `host:port` (subject to the same SSRF policy as [`guard_url`]).
    Tcp { host: String, port: u16 },
    /// A local Unix-domain socket path.
    Unix { path: String },
}

/// An opened connection. Read/write are async methods (rather than exposing the concrete stream) so
/// the caller — the plugin host's `conn.*` capability — can shuttle bytes uniformly over TCP or Unix.
pub enum DialStream {
    /// A TCP stream.
    Tcp(tokio::net::TcpStream),
    /// A Unix-domain stream.
    #[cfg(unix)]
    Unix(tokio::net::UnixStream),
}

impl DialStream {
    /// Read up to `max` bytes; an empty `Vec` signals EOF.
    pub async fn read(&mut self, max: usize) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; max];
        let n = match self {
            DialStream::Tcp(s) => s.read(&mut buf).await,
            #[cfg(unix)]
            DialStream::Unix(s) => s.read(&mut buf).await,
        }
        .map_err(|e| Error::Other(format!("conn read: {e}")))?;
        buf.truncate(n);
        Ok(buf)
    }

    /// Write all of `data`.
    pub async fn write_all(&mut self, data: &[u8]) -> Result<()> {
        match self {
            DialStream::Tcp(s) => s.write_all(data).await,
            #[cfg(unix)]
            DialStream::Unix(s) => s.write_all(data).await,
        }
        .map_err(|e| Error::Other(format!("conn write: {e}")))
    }

    /// Shut the connection down.
    pub async fn shutdown(&mut self) -> Result<()> {
        match self {
            DialStream::Tcp(s) => s.shutdown().await,
            #[cfg(unix)]
            DialStream::Unix(s) => s.shutdown().await,
        }
        .map_err(|e| Error::Other(format!("conn shutdown: {e}")))
    }
}

/// Dial a socket target, applying the SSRF egress policy to TCP unless `allow_private`.
pub async fn dial(target: &DialTarget, allow_private: bool) -> Result<DialStream> {
    match target {
        DialTarget::Tcp { host, port } => {
            guard_target_host(host, *port, allow_private)?;
            let s = tokio::net::TcpStream::connect((host.as_str(), *port))
                .await
                .map_err(|e| Error::Other(format!("tcp dial {host}:{port}: {e}")))?;
            Ok(DialStream::Tcp(s))
        }
        DialTarget::Unix { path } => {
            #[cfg(unix)]
            {
                let s = tokio::net::UnixStream::connect(path)
                    .await
                    .map_err(|e| Error::Other(format!("unix dial {path}: {e}")))?;
                Ok(DialStream::Unix(s))
            }
            #[cfg(not(unix))]
            {
                let _ = path;
                Err(Error::Other(
                    "unix-socket dial is unsupported on this platform".into(),
                ))
            }
        }
    }
}

/// Guard a `host:port` for a socket dial with the same policy as [`guard_url`]: internal hostnames and
/// private/loopback/link-local IPs are blocked unless `allow_private`.
fn guard_target_host(host: &str, port: u16, allow_private: bool) -> Result<()> {
    if allow_private {
        return Ok(());
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return block_if(ip, host);
    }
    let lower = host.to_ascii_lowercase();
    if lower == "localhost"
        || lower.ends_with(".localhost")
        || lower == "metadata.google.internal"
        || lower.ends_with(".internal")
    {
        return Err(Error::Other(format!(
            "refusing to dial internal host {host}"
        )));
    }
    if let Ok(addrs) = (host, port).to_socket_addrs() {
        for sa in addrs {
            block_if(sa.ip(), host)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_public() {
        assert!(guard_url("https://example.com/path", false).is_ok());
        assert!(guard_url("http://93.184.216.34/", false).is_ok());
    }

    #[test]
    fn blocks_schemes() {
        assert!(guard_url("file:///etc/passwd", false).is_err());
        assert!(guard_url("ftp://example.com", false).is_err());
    }

    #[test]
    fn blocks_private_and_loopback_literals() {
        for u in [
            "http://localhost:8080",
            "http://127.0.0.1/",
            "http://10.0.0.5/",
            "http://192.168.1.1/",
            "http://169.254.169.254/latest/meta-data/", // cloud metadata
            "http://100.100.0.1/",                      // CGNAT
            "http://0.0.0.0/",
        ] {
            assert!(guard_url(u, false).is_err(), "should block {u}");
        }
    }

    #[test]
    fn blocks_ipv6_internal_and_mapped() {
        assert!(guard_url("http://[::1]/", false).is_err());
        assert!(
            guard_url("http://[::ffff:169.254.169.254]/", false).is_err(),
            "IPv4-mapped metadata address must be blocked"
        );
        assert!(
            guard_url("http://[fd00::1]/", false).is_err(),
            "unique-local IPv6 must be blocked"
        );
    }

    #[test]
    fn blocks_internal_hostnames() {
        assert!(guard_url("http://metadata.google.internal/", false).is_err());
        assert!(guard_url("http://anything.internal/", false).is_err());
    }

    #[test]
    fn allow_private_opt_in() {
        assert!(guard_url("http://127.0.0.1/", true).is_ok());
    }

    #[tokio::test]
    async fn dial_tcp_round_trips_and_guards_private() {
        // A loopback echo server (hermetic — no external network).
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 64];
                if let Ok(n) = sock.read(&mut buf).await {
                    let _ = sock.write_all(&buf[..n]).await;
                }
            }
        });

        // Loopback is blocked by the egress policy unless `allow_private`.
        let target = DialTarget::Tcp {
            host: "127.0.0.1".into(),
            port,
        };
        assert!(
            dial(&target, false).await.is_err(),
            "loopback must be guarded"
        );

        // With `allow_private`, the dial round-trips.
        let mut s = dial(&target, true).await.unwrap();
        s.write_all(b"ping").await.unwrap();
        let got = s.read(64).await.unwrap();
        assert_eq!(&got, b"ping");
        s.shutdown().await.ok();
    }
}
