//! Network egress guard — the single SSRF policy reused by every surface that fetches a URL
//! (`flux-capabilities`' `browser` web.fetch, `flux-plugin`'s `http.do`).
//!
//! Beyond rejecting non-HTTP schemes and raw private/loopback IP literals, this **resolves the
//! host to IP addresses** and blocks the request if any resolved address is private, loopback,
//! link-local, unique-local, CGNAT, or an IPv4-mapped form of those — so a hostname pointing at
//! `169.254.169.254` (cloud metadata) or `[::ffff:10.0.0.1]` can't slip through. DNS rebinding
//! (a different answer at connect time) is still possible; this is defense-in-depth, not a
//! complete TOCTOU fix.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs};

use flux_core::{Error, Result};

/// What one egress caller is allowed to reach beyond public addresses.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PrivateNetAllow {
    /// Full SSRF guard: private/loopback/link-local/internal hosts are refused.
    #[default]
    None,
    /// Only these host patterns may resolve to private addresses.
    Hosts(Vec<String>),
    /// This one caller may reach any private address.
    Any,
}

impl PrivateNetAllow {
    pub fn from_legacy_bool(allow: bool) -> Self {
        if allow {
            Self::Any
        } else {
            Self::None
        }
    }

    pub fn from_hosts(hosts: impl IntoIterator<Item = String>) -> Self {
        let hosts = hosts
            .into_iter()
            .map(|h| h.trim().to_ascii_lowercase())
            .filter(|h| !h.is_empty())
            .collect::<Vec<_>>();
        if hosts.iter().any(|h| h == "*") {
            Self::Any
        } else if hosts.is_empty() {
            Self::None
        } else {
            Self::Hosts(hosts)
        }
    }

    pub fn allows_host(&self, host: &str) -> bool {
        match self {
            Self::Any => true,
            Self::None => false,
            Self::Hosts(patterns) => host_matches(patterns, host),
        }
    }
}

/// DNS resolution seam used by the URL guard. Production uses [`SystemHostResolver`]; callers that
/// store destinations can inject a deterministic resolver in tests to prove registration and
/// delivery each resolve independently.
pub trait HostResolver: Send + Sync {
    fn resolve(&self, host: &str, port: u16) -> std::io::Result<Vec<IpAddr>>;
}

/// Host resolver backed by the operating system's configured DNS implementation.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemHostResolver;

impl HostResolver for SystemHostResolver {
    fn resolve(&self, host: &str, port: u16) -> std::io::Result<Vec<IpAddr>> {
        (host, port)
            .to_socket_addrs()
            .map(|addresses| addresses.map(|address| address.ip()).collect())
    }
}

/// Reject URLs that aren't safe to fetch. Private addresses are allowed only when `allow` covers the
/// URL host for this caller.
pub fn guard_url_scoped(raw: &str, allow: &PrivateNetAllow) -> Result<url::Url> {
    guard_url_scoped_with_resolver(raw, allow, &SystemHostResolver)
}

/// Resolve and guard a URL with an explicit DNS resolver. If a hostname has multiple answers, one
/// blocked address rejects the whole destination. Resolution failures remain connection errors,
/// not grants; the eventual client will fail to connect.
pub fn guard_url_scoped_with_resolver(
    raw: &str,
    allow: &PrivateNetAllow,
    resolver: &dyn HostResolver,
) -> Result<url::Url> {
    guard_and_pin(raw, allow, resolver).map(|(url, _)| url)
}

/// Like [`guard_url_scoped`], but also returns the exact socket addresses the guard vetted so the
/// caller can **pin** the connection to them. Without pinning, reqwest re-resolves the hostname at
/// connect time, and a low-TTL attacker host can answer a public address to this guard and an
/// internal one (`169.254.169.254`, RFC1918, …) at connect — a DNS-rebinding SSRF that reaches cloud
/// metadata. Pinning the connection to the vetted set closes that TOCTOU. An IP-literal URL pins to
/// that literal; an unresolvable host yields an empty set (resolution deferred to the client — safe,
/// as it never reached an internal target here). See story C-77.
pub fn guard_url_scoped_pinned(
    raw: &str,
    allow: &PrivateNetAllow,
) -> Result<(url::Url, Vec<SocketAddr>)> {
    guard_and_pin(raw, allow, &SystemHostResolver)
}

/// [`guard_url_scoped_pinned`] with an injectable resolver — tests inject a rebinding resolver to
/// prove the connection pins to the vetted answer rather than a later, internal one.
pub fn guard_url_scoped_pinned_with_resolver(
    raw: &str,
    allow: &PrivateNetAllow,
    resolver: &dyn HostResolver,
) -> Result<(url::Url, Vec<SocketAddr>)> {
    guard_and_pin(raw, allow, resolver)
}

/// Shared core: parse + scheme-check + SSRF-vet, returning the URL and the vetted socket addresses.
fn guard_and_pin(
    raw: &str,
    allow: &PrivateNetAllow,
    resolver: &dyn HostResolver,
) -> Result<(url::Url, Vec<SocketAddr>)> {
    let url = url::Url::parse(raw).map_err(|e| Error::Other(format!("invalid url: {e}")))?;
    match url.scheme() {
        "http" | "https" => {}
        other => return Err(Error::Other(format!("unsupported url scheme: {other}"))),
    }
    let port = url.port_or_known_default().unwrap_or(80);
    // `Host` parses literal IPs into typed addresses (so an IPv6 literal isn't a bracketed string).
    match url.host() {
        None => Err(Error::Other("url has no host".into())),
        Some(url::Host::Ipv4(v4)) => {
            block_if(IpAddr::V4(v4), &v4.to_string(), allow)?;
            Ok((url, vec![SocketAddr::new(IpAddr::V4(v4), port)]))
        }
        Some(url::Host::Ipv6(v6)) => {
            block_if(IpAddr::V6(v6), &v6.to_string(), allow)?;
            Ok((url, vec![SocketAddr::new(IpAddr::V6(v6), port)]))
        }
        Some(url::Host::Domain(domain)) => {
            // Block internal hostnames outright (these often front link-local metadata services).
            let lower = domain.to_ascii_lowercase();
            if is_internal_hostname(&lower) && !allow.allows_host(domain) {
                return Err(Error::Other(format!(
                    "refusing to fetch internal host {domain}"
                )));
            }
            // Resolve to IPs and reject if ANY resolved address is in a blocked range. Collect the
            // vetted addresses so the caller can pin to exactly them. An unresolvable host is left to
            // fail at connect time (it's not an SSRF) with an empty pin set.
            let mut pinned = Vec::new();
            if let Ok(addresses) = resolver.resolve(domain, port) {
                for address in addresses {
                    block_if(address, domain, allow)?;
                    pinned.push(SocketAddr::new(address, port));
                }
            }
            Ok((url, pinned))
        }
    }
}

/// Compatibility wrapper for callers that still need the old all-or-nothing shape.
pub fn guard_url(raw: &str, allow_private: bool) -> Result<url::Url> {
    guard_url_scoped(raw, &PrivateNetAllow::from_legacy_bool(allow_private))
}

/// Whether `host` is (or resolves to) a private/internal address — i.e. one the SSRF guard would
/// block absent a grant. Reuses the same IP-range and internal-hostname rules as [`guard_url_scoped`]
/// so callers (e.g. the plugin host's egress-admit audit) classify an admitted host identically to
/// how it was guarded. An unresolvable host is treated as not-private (it isn't an internal target).
pub fn host_resolves_private(host: &str) -> bool {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return is_blocked_ip(ip);
    }
    let lower = host.to_ascii_lowercase();
    if is_internal_hostname(&lower) {
        return true;
    }
    // Resolve and treat the host as private if ANY resolved address is in a blocked range. Use a
    // throwaway port — only the IPs matter. `to_socket_addrs` needs a port, hence `(host, 0)`.
    if let Ok(addrs) = (host, 0u16).to_socket_addrs() {
        return addrs.into_iter().any(|sa| is_blocked_ip(sa.ip()));
    }
    false
}

/// `Err` if `ip` is in a range the agent may never reach (SSRF protection); `Ok(())` otherwise.
fn block_if(ip: IpAddr, host: &str, allow: &PrivateNetAllow) -> Result<()> {
    if is_blocked_ip(ip) && !allow.allows_host(host) {
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

/// Dial a socket target, applying the SSRF egress policy to TCP unless `allow` covers the host.
pub async fn dial_scoped(target: &DialTarget, allow: &PrivateNetAllow) -> Result<DialStream> {
    match target {
        DialTarget::Tcp { host, port } => {
            guard_target_host(host, *port, allow)?;
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

/// Compatibility wrapper for callers that still need the old all-or-nothing shape.
pub async fn dial(target: &DialTarget, allow_private: bool) -> Result<DialStream> {
    dial_scoped(target, &PrivateNetAllow::from_legacy_bool(allow_private)).await
}

/// Guard a `host:port` for a socket dial with the same policy as [`guard_url`]: internal hostnames and
/// private/loopback/link-local IPs are blocked unless `allow` covers this host.
fn guard_target_host(host: &str, port: u16, allow: &PrivateNetAllow) -> Result<()> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return block_if(ip, host, allow);
    }
    let lower = host.to_ascii_lowercase();
    if is_internal_hostname(&lower) && !allow.allows_host(host) {
        return Err(Error::Other(format!(
            "refusing to dial internal host {host}"
        )));
    }
    if let Ok(addrs) = (host, port).to_socket_addrs() {
        for sa in addrs {
            block_if(sa.ip(), host, allow)?;
        }
    }
    Ok(())
}

fn is_internal_hostname(lower: &str) -> bool {
    lower == "localhost"
        || lower.ends_with(".localhost")
        || lower == "metadata.google.internal"
        || lower.ends_with(".internal")
}

fn host_matches(patterns: &[String], host: &str) -> bool {
    let host = host
        .trim()
        .trim_matches('[')
        .trim_matches(']')
        .to_ascii_lowercase();
    patterns.iter().any(|p| {
        let p = p
            .trim()
            .trim_matches('[')
            .trim_matches(']')
            .to_ascii_lowercase();
        p == "*"
            || p == host
            || p.strip_prefix("*.").is_some_and(|suffix| {
                host.ends_with(suffix)
                    && host.len() > suffix.len()
                    && host.as_bytes()[host.len() - suffix.len() - 1] == b'.'
            })
    })
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

    #[test]
    fn scoped_private_allow_is_host_specific() {
        let loopback = PrivateNetAllow::from_hosts(vec!["127.0.0.1".to_string()]);
        assert!(guard_url_scoped("http://127.0.0.1/", &loopback).is_ok());
        assert!(guard_url_scoped("http://10.0.0.5/", &loopback).is_err());

        let localhost = PrivateNetAllow::from_hosts(vec!["localhost".to_string()]);
        assert!(guard_url_scoped("http://localhost:8080/", &localhost).is_ok());
        assert!(guard_url_scoped("http://metadata.google.internal/", &localhost).is_err());
    }

    struct FixedResolver(Vec<IpAddr>);

    impl HostResolver for FixedResolver {
        fn resolve(&self, _host: &str, _port: u16) -> std::io::Result<Vec<IpAddr>> {
            Ok(self.0.clone())
        }
    }

    /// C-77: the pinned guard returns the exact vetted socket address, so the egress layer connects
    /// to *it* rather than a value re-resolved at connect. A rebinding host that answers a public IP
    /// here therefore has its connection pinned to that public IP — a later internal answer at
    /// connect never gets a chance to be dialed.
    #[test]
    fn pinned_guard_returns_the_vetted_address_to_pin_the_connection() {
        let vetted: IpAddr = "93.184.216.34".parse().unwrap();
        let (url, pinned) = guard_url_scoped_pinned_with_resolver(
            "https://rebinding.example/hook",
            &PrivateNetAllow::None,
            &FixedResolver(vec![vetted]),
        )
        .expect("a public answer is admitted");
        assert_eq!(url.host_str(), Some("rebinding.example"));
        assert_eq!(
            pinned,
            vec![SocketAddr::new(vetted, 443)],
            "the connection must be pinned to the vetted address and port"
        );

        // An IP-literal URL pins to that literal, at its explicit port.
        let (_u, literal) =
            guard_url_scoped_pinned("http://93.184.216.34:8080/", &PrivateNetAllow::None).unwrap();
        assert_eq!(
            literal,
            vec![SocketAddr::new("93.184.216.34".parse().unwrap(), 8080)]
        );

        // Pinning does not weaken the vet: a blocked resolved address still rejects the destination.
        assert!(guard_url_scoped_pinned_with_resolver(
            "https://rebinding.example/hook",
            &PrivateNetAllow::None,
            &FixedResolver(vec!["169.254.169.254".parse().unwrap()]),
        )
        .is_err());
    }

    #[test]
    fn dns_resolution_matrix_blocks_every_internal_family_and_mixed_answers() {
        let blocked = [
            "127.0.0.1",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:10.0.0.1",
        ];
        for address in blocked {
            let resolver = FixedResolver(vec![address.parse().unwrap()]);
            assert!(
                guard_url_scoped_with_resolver(
                    "https://rebinding.example/hook",
                    &PrivateNetAllow::None,
                    &resolver,
                )
                .is_err(),
                "hostname resolution admitted {address}"
            );
        }

        let mixed = FixedResolver(vec![
            "93.184.216.34".parse().unwrap(),
            "169.254.169.254".parse().unwrap(),
        ]);
        assert!(guard_url_scoped_with_resolver(
            "https://rebinding.example/hook",
            &PrivateNetAllow::None,
            &mixed,
        )
        .is_err());

        let exact = PrivateNetAllow::from_hosts(["rebinding.example".to_string()]);
        assert!(
            guard_url_scoped_with_resolver("https://rebinding.example/hook", &exact, &mixed,)
                .is_ok()
        );
        let wrong = PrivateNetAllow::from_hosts(["other.example".to_string()]);
        assert!(
            guard_url_scoped_with_resolver("https://rebinding.example/hook", &wrong, &mixed,)
                .is_err()
        );
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
