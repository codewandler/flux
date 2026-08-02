//! Network egress guard — the single SSRF policy reused by every surface that fetches a URL
//! (`flux-capabilities`' `browser` web.fetch, `flux-plugin`'s `http.do`).
//!
//! Beyond rejecting non-HTTP schemes and raw private/loopback IP literals, this **resolves the
//! host to IP addresses** and blocks the request if any resolved address is private, loopback,
//! link-local, unique-local, CGNAT, or an IPv4-mapped form of those — so a hostname pointing at
//! `169.254.169.254` (cloud metadata) or `[::ffff:10.0.0.1]` can't slip through. Callers that
//! consume a pinned guard/dial result bind the connection to the vetted addresses; callers that
//! use only a URL-returning compatibility API do not receive that DNS-rebinding guarantee.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};

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

/// Guard a URL for a request that may carry a destination-scoped secret.
///
/// Unlike the raw `(Url, pins)` API, this also mints an unforgeable
/// [`GuardedSecretTarget`](crate::secret_scope::GuardedSecretTarget): its private destination token
/// can only originate here, after this guard resolved and vetted the addresses. The caller receives
/// the URL, pins and token as one correlated value and must use the same pins for the connection.
pub fn guard_url_scoped_for_secret(
    raw: &str,
    allow: &PrivateNetAllow,
) -> Result<crate::secret_scope::GuardedSecretTarget> {
    let (url, pinned) = guard_and_pin(raw, allow, &SystemHostResolver)?;
    Ok(crate::secret_scope::GuardedSecretTarget::from_guard(
        url, pinned,
    ))
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
//
// C-396 extended the dialer to datagram (UDP) and raw (ICMP) targets for reachability checks and
// protocol probes. They reuse `guard_target_host_pinned` — the *same* resolution and range checks
// TCP runs — rather than re-deriving the policy; there is exactly one egress guard in this crate.
//
// WHERE THIS STOPS, for whoever implements the model-facing ops. This is the substrate primitive:
// a guarded destination and a socket pinned to it. It is deliberately NOT the operation surface.
// The `network-primitives` epic owns that — C-285 DNS, C-286 TCP, C-287 UDP, C-288 ICMP — and each
// of those is a registered `ToolSpec` with intent/policy mapping, bounded datagram counts and
// timeouts, multicast/broadcast and spoofing rules, and per-reply validation, all gated on C-284's
// design. TCP is the precedent: `DialTarget::Tcp` has existed since D-12 and C-286 is still unbuilt,
// because the two are different layers rather than two names for one job. **Build those ops on this
// primitive; do not re-derive the guard inside one.** What you already get here and need not redo:
// the destination is vetted once and the socket is `connect`ed to it, so the kernel enforces both
// the send destination and the reply source.
// ---------------------------------------------------------------------------

use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A socket target to dial.
///
/// Every IP variant is subject to the one egress policy: the host is resolved once, every answer is
/// range-checked, and the vetted addresses are what gets dialled. Unix sockets are local and gated
/// by capability, not by this policy.
#[derive(Debug, Clone)]
pub enum DialTarget {
    /// A TCP `host:port` (subject to the same SSRF policy as [`guard_url`]).
    Tcp { host: String, port: u16 },
    /// A local Unix-domain socket path.
    Unix { path: String },
    /// A UDP `host:port` — for reachability checks and datagram protocol probes. Guarded exactly as
    /// [`DialTarget::Tcp`] is; the resulting socket is *connected* to the vetted peer, so the kernel
    /// itself refuses to carry a datagram anywhere else.
    Udp { host: String, port: u16 },
    /// A raw ICMP destination — no port, because ICMP has none. Guarded exactly as
    /// [`DialTarget::Tcp`] is, and **additionally** requires the privilege to open a raw socket
    /// (`CAP_NET_RAW` on Linux, root on macOS). That privilege is checked when the target is dialled
    /// and before any datagram is addressed: see [`RawIcmpOpener`].
    Icmp { host: String },
}

/// An opened connection. Read/write are async methods (rather than exposing the concrete stream) so
/// the caller — the plugin host's `conn.*` capability — can shuttle bytes uniformly over TCP or Unix.
///
/// The datagram variants ([`DialStream::Udp`], [`DialStream::Icmp`]) keep the same method names but
/// **not** stream semantics: one `write_all` is one datagram and one `read` is one datagram. Two
/// consequences a caller must know:
///
/// - A zero-length datagram is indistinguishable from the EOF an empty `read` signals on a stream.
///   A caller that must tell them apart should not use this shim.
/// - Truncation is an error in **both** directions — a short send from `write_all` and a datagram
///   too large for `read`'s buffer both fail loudly, because neither has a second call that
///   completes it and a silently beheaded reply cannot be validated.
pub enum DialStream {
    /// A TCP stream.
    Tcp(tokio::net::TcpStream),
    /// A Unix-domain stream.
    #[cfg(unix)]
    Unix(tokio::net::UnixStream),
    /// A UDP socket connected to the vetted peer.
    Udp(tokio::net::UdpSocket),
    /// A raw ICMP socket connected to the vetted peer.
    Icmp(IcmpSocket),
}

impl DialStream {
    /// Read up to `max` bytes; an empty `Vec` signals EOF (or, on a datagram variant, an empty
    /// datagram — see the type-level note).
    ///
    /// On a datagram variant a datagram that does not fit in `max` is an **error**, not a silent
    /// beheading: the kernel discards the remainder at `recv`, and neither this shim nor the caller
    /// can recover it. Read again with a larger buffer.
    pub async fn read(&mut self, max: usize) -> Result<Vec<u8>> {
        // Datagram reads ask the kernel for one byte more than the caller wanted. `recv` fills what
        // it can and reports what it wrote, so `n > max` is exactly "the datagram did not fit" —
        // portable, and without it an oversized reply is indistinguishable from a reply that really
        // was `max` bytes long. A caller validating a reply (C-287/C-288) has to be able to tell
        // those apart. Stream variants keep the exact buffer: over-reading would consume bytes the
        // caller did not ask for.
        let datagram = matches!(self, DialStream::Udp(_) | DialStream::Icmp(_));
        let mut buf = vec![0u8; if datagram { max.saturating_add(1) } else { max }];
        let n = match self {
            DialStream::Tcp(s) => s.read(&mut buf).await,
            #[cfg(unix)]
            DialStream::Unix(s) => s.read(&mut buf).await,
            DialStream::Udp(s) => s.recv(&mut buf).await,
            DialStream::Icmp(s) => s.recv(&mut buf).await,
        }
        .map_err(|e| Error::Other(format!("conn read: {e}")))?;
        if datagram && n > max {
            return Err(Error::Other(format!(
                "conn read: datagram exceeds the {max}-byte read buffer — the kernel discarded the \
                 remainder, so the reply cannot be validated; read with a larger buffer"
            )));
        }
        buf.truncate(n);
        Ok(buf)
    }

    /// Write all of `data`. On a datagram variant this is a single datagram: a short write is an
    /// error rather than a silent partial send, because there is no second write that completes it.
    pub async fn write_all(&mut self, data: &[u8]) -> Result<()> {
        let sent = match self {
            DialStream::Tcp(s) => return finish_write(s.write_all(data).await),
            #[cfg(unix)]
            DialStream::Unix(s) => return finish_write(s.write_all(data).await),
            DialStream::Udp(s) => s.send(data).await,
            DialStream::Icmp(s) => s.send(data).await,
        }
        .map_err(|e| Error::Other(format!("conn write: {e}")))?;
        if sent != data.len() {
            return Err(Error::Other(format!(
                "conn write: datagram truncated ({sent} of {} bytes sent)",
                data.len()
            )));
        }
        Ok(())
    }

    /// Shut the connection down. A connected datagram socket has nothing to shut down — closing it
    /// is dropping it — so the datagram variants succeed without a syscall.
    pub async fn shutdown(&mut self) -> Result<()> {
        match self {
            DialStream::Tcp(s) => s.shutdown().await,
            #[cfg(unix)]
            DialStream::Unix(s) => s.shutdown().await,
            DialStream::Udp(_) | DialStream::Icmp(_) => return Ok(()),
        }
        .map_err(|e| Error::Other(format!("conn shutdown: {e}")))
    }
}

fn finish_write(outcome: std::io::Result<()>) -> Result<()> {
    outcome.map_err(|e| Error::Other(format!("conn write: {e}")))
}

/// A raw ICMP socket connected to one vetted peer.
///
/// Held as a connected datagram socket, which is what makes the pin structural: the peer was fixed
/// when the socket was opened, so no later call can address a different destination. Note the
/// platform asymmetry a caller must handle itself: a raw IPv4 socket delivers received datagrams
/// **including** their IP header, while a raw IPv6 socket does not.
pub struct IcmpSocket {
    socket: tokio::net::UdpSocket,
    peer: SocketAddr,
}

impl IcmpSocket {
    /// The vetted address this socket is connected to.
    pub fn peer(&self) -> SocketAddr {
        self.peer
    }

    /// Send one ICMP datagram to the connected peer. The caller builds the ICMP message itself —
    /// this crate guards the destination, it does not speak the protocol.
    pub async fn send(&self, data: &[u8]) -> std::io::Result<usize> {
        self.socket.send(data).await
    }

    /// Receive one datagram from the connected peer.
    pub async fn recv(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.socket.recv(buf).await
    }
}

impl std::fmt::Debug for IcmpSocket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IcmpSocket")
            .field("peer", &self.peer)
            .finish()
    }
}

/// The seam that opens a raw ICMP socket, so the privilege question is injectable.
///
/// Production uses [`SystemRawIcmp`]. It exists as a trait for two reasons, both about the
/// capability rather than about testing convenience:
///
/// - **The refusal is testable on either kind of machine.** Whether this process holds `CAP_NET_RAW`
///   is a property of the deployment, so a test that asserted the real refusal would pass or fail
///   depending on who ran it.
/// - **The refusal is produced here, not by the implementor.** An implementation reports the raw
///   [`std::io::Error`] it got from the kernel; this module turns [`std::io::ErrorKind::PermissionDenied`]
///   into a message naming the capability. An implementor cannot weaken or reword that.
///
/// An implementation **must not transmit**. Opening and connecting a datagram socket contacts
/// nothing, which is precisely why the privilege check can happen at construction: a check deferred
/// to the first send is a check that already leaked the attempt to the destination.
pub trait RawIcmpOpener: Send + Sync {
    /// Open a raw ICMP socket for `peer`'s address family and connect it to `peer`.
    ///
    /// The returned socket is registered with the ambient async reactor, so an implementation may
    /// require a [`tokio`] runtime context. It must **return** that as an error rather than panic —
    /// [`SystemRawIcmp`] checks for a runtime before it opens anything.
    ///
    /// An implementation must also create the descriptor **close-on-exec**, atomically where the
    /// platform allows it. This process spawns children concurrently, so a descriptor that is only
    /// marked after creation is inheritable for the width of that window.
    fn open(&self, peer: SocketAddr) -> std::io::Result<IcmpSocket>;
}

/// Raw-ICMP opener backed by the operating system's socket API.
///
/// **Platform reality, and what flux does about it.** A raw ICMP socket is
/// `socket(AF_INET, SOCK_RAW, IPPROTO_ICMP)` (or `AF_INET6`/`IPPROTO_ICMPV6`). On Linux that
/// requires `CAP_NET_RAW`; on macOS it requires root. Linux also offers an *unprivileged* ICMP
/// socket (`SOCK_DGRAM`/`IPPROTO_ICMP`, enabled per-gid by `net.ipv4.ping_group_range`), and macOS
/// offers it to everyone — **flux deliberately does not fall back to it.** It is a differently
/// privileged path with different wire semantics (the kernel owns the echo identifier and rewrites
/// it), so a fallback would silently change what a probe measures depending on the host it ran on.
/// An absent privilege is refused, named, and left to the operator.
///
/// A process confined by an OS sandbox with the network closed sees the same shape: the `socket`
/// call fails, the dial is refused before anything is addressed, and the error carries the errno.
///
/// **Runtime.** `open` registers the socket with the async reactor, so it must be called from
/// within a [`tokio`] runtime. It returns an error when it is not, rather than panicking the way a
/// bare `UdpSocket::from_std` would.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemRawIcmp;

/// `SOCK_RAW`, with `SOCK_CLOEXEC` folded in on every platform whose `socket(2)` accepts it.
///
/// **This is the atomic form, and it is the only one that actually closes the hole.** A descriptor
/// created without `SOCK_CLOEXEC` and marked close-on-exec by a following `fcntl` is *inheritable
/// for the width of that window*: flux runs a multithreaded runtime and spawns children
/// concurrently (`System::build_command`), and `std::process::Command` closes no inherited
/// descriptors — so a `fork`+`exec` on another thread inside the window hands the child a raw
/// network socket that traversed no grant, bypassing the guarded-spawn chokepoint entirely. std
/// itself creates every socket this way for the same reason.
///
/// Apple platforms have no such flag, so `open` falls back to the `fcntl` there. That narrows the
/// window; it does not close it, and nothing portable can.
#[cfg(unix)]
const fn raw_socket_type() -> libc::c_int {
    #[cfg(any(
        target_os = "android",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "illumos",
        target_os = "linux",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "solaris",
    ))]
    {
        libc::SOCK_RAW | libc::SOCK_CLOEXEC
    }
    #[cfg(not(any(
        target_os = "android",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "illumos",
        target_os = "linux",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "solaris",
    )))]
    {
        libc::SOCK_RAW
    }
}

#[cfg(unix)]
impl RawIcmpOpener for SystemRawIcmp {
    fn open(&self, peer: SocketAddr) -> std::io::Result<IcmpSocket> {
        use std::os::fd::FromRawFd;

        // `UdpSocket::from_std` below panics outside a runtime. A published API returns that rather
        // than aborting its caller — and checking first means no raw socket is created only to be
        // dropped.
        if tokio::runtime::Handle::try_current().is_err() {
            return Err(std::io::Error::other(
                "a raw ICMP socket must be opened inside a tokio runtime",
            ));
        }

        let (domain, protocol) = match peer.ip() {
            IpAddr::V4(_) => (libc::AF_INET, libc::IPPROTO_ICMP),
            IpAddr::V6(_) => (libc::AF_INET6, libc::IPPROTO_ICMPV6),
        };
        // SAFETY: `socket` is a plain syscall over three integers, returning a fresh owned
        // descriptor or -1. It transmits nothing — an unprivileged process is refused right here,
        // which is the whole point of checking at construction. See `raw_socket_type` for why
        // close-on-exec is requested *in this call* rather than set afterwards.
        let fd = unsafe { libc::socket(domain, raw_socket_type(), protocol) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `fd` is a fresh descriptor owned by nothing else, so handing ownership to a
        // `UdpSocket` is sound and makes it responsible for closing it — including on every error
        // path below. `UdpSocket` is a thin wrapper over a datagram descriptor and a raw ICMP socket
        // is one; borrowing it here is what gives us portable `connect`/`send`/`recv` instead of a
        // hand-built `sockaddr`.
        let socket = unsafe { std::net::UdpSocket::from_raw_fd(fd) };
        // On a platform without `SOCK_CLOEXEC` this is the only mechanism available, and the window
        // above it is real. Elsewhere it is a cheap reassertion of a flag `socket` already set.
        // SAFETY: `fd` is still owned by `socket`, which outlives this call.
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        socket.set_nonblocking(true)?;
        // `connect` on a datagram socket only fixes the peer — it transmits nothing — and it is what
        // pins this socket to the vetted address in the kernel.
        socket.connect(peer)?;
        Ok(IcmpSocket {
            socket: tokio::net::UdpSocket::from_std(socket)?,
            peer,
        })
    }
}

#[cfg(not(unix))]
impl RawIcmpOpener for SystemRawIcmp {
    fn open(&self, peer: SocketAddr) -> std::io::Result<IcmpSocket> {
        let _ = peer;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "raw ICMP sockets are not supported on this platform",
        ))
    }
}

/// Dial a socket target, applying the SSRF egress policy to every IP variant (TCP, UDP, ICMP)
/// unless `allow` covers the host.
pub async fn dial_scoped(target: &DialTarget, allow: &PrivateNetAllow) -> Result<DialStream> {
    dial_scoped_pinned(target, allow)
        .await
        .map(|(stream, _)| stream)
}

/// Guard and dial a socket target, returning the exact addresses that were vetted and attempted.
///
/// Hostnames are resolved exactly once. The resulting addresses are policy-checked and then passed
/// directly to the connect call — [`tokio::net::TcpStream::connect`] for TCP, and a `connect` on the
/// datagram socket for UDP and ICMP — so a hostname cannot answer publicly to the guard and rebind
/// to a private address when the connection is opened. An empty or failed DNS answer is refused
/// rather than re-resolved by the connect path. Unix sockets have no DNS address set and return an
/// empty vector; ICMP has no port and pins its addresses at port 0.
pub async fn dial_scoped_pinned(
    target: &DialTarget,
    allow: &PrivateNetAllow,
) -> Result<(DialStream, Vec<SocketAddr>)> {
    dial_scoped_pinned_with_resolver(target, allow, &SystemHostResolver).await
}

/// [`dial_scoped_pinned`] with an injectable resolver for deterministic rebinding tests.
pub async fn dial_scoped_pinned_with_resolver(
    target: &DialTarget,
    allow: &PrivateNetAllow,
    resolver: &dyn HostResolver,
) -> Result<(DialStream, Vec<SocketAddr>)> {
    dial_scoped_pinned_with_icmp_opener(target, allow, resolver, &SystemRawIcmp).await
}

/// [`dial_scoped_pinned_with_resolver`] with the raw-ICMP socket seam injectable too.
///
/// Only [`DialTarget::Icmp`] consults `icmp`, and only *after* the egress guard has approved the
/// destination — a blocked address is refused whether or not this process could have opened a raw
/// socket, and an unprivileged process is refused before one is opened at all.
pub async fn dial_scoped_pinned_with_icmp_opener(
    target: &DialTarget,
    allow: &PrivateNetAllow,
    resolver: &dyn HostResolver,
    icmp: &dyn RawIcmpOpener,
) -> Result<(DialStream, Vec<SocketAddr>)> {
    match target {
        DialTarget::Tcp { host, port } => {
            let pinned =
                vetted_or_refuse(host, *port, allow, resolver, &format!("tcp {host}:{port}"))?;
            let mut last_error = None;
            for address in pinned.iter().copied() {
                match tokio::net::TcpStream::connect(address).await {
                    Ok(stream) => return Ok((DialStream::Tcp(stream), pinned)),
                    Err(error) => last_error = Some(error),
                }
            }
            Err(Error::Other(format!(
                "tcp dial {host}:{port}: {}",
                describe_dial_failure(last_error)
            )))
        }
        DialTarget::Unix { path } => {
            #[cfg(unix)]
            {
                let s = tokio::net::UnixStream::connect(path)
                    .await
                    .map_err(|e| Error::Other(format!("unix dial {path}: {e}")))?;
                Ok((DialStream::Unix(s), Vec::new()))
            }
            #[cfg(not(unix))]
            {
                let _ = path;
                Err(Error::Other(
                    "unix-socket dial is unsupported on this platform".into(),
                ))
            }
        }
        DialTarget::Udp { host, port } => {
            let pinned =
                vetted_or_refuse(host, *port, allow, resolver, &format!("udp {host}:{port}"))?;
            let mut last_error = None;
            for address in pinned.iter().copied() {
                // Bind an ephemeral local port in the peer's family, then connect. UDP `connect`
                // only fixes the peer — it transmits nothing — and it is what confines every later
                // datagram to the address the guard approved.
                match tokio::net::UdpSocket::bind(unspecified_for(address)).await {
                    Ok(socket) => match socket.connect(address).await {
                        Ok(()) => return Ok((DialStream::Udp(socket), pinned)),
                        Err(error) => last_error = Some(error),
                    },
                    Err(error) => last_error = Some(error),
                }
            }
            Err(Error::Other(format!(
                "udp dial {host}:{port}: {}",
                describe_dial_failure(last_error)
            )))
        }
        DialTarget::Icmp { host } => {
            // ICMP has no port, so the vetted set pins addresses at port 0.
            let pinned = vetted_or_refuse(host, 0, allow, resolver, &format!("icmp {host}"))?;
            let mut last_error = None;
            for address in pinned.iter().copied() {
                match icmp.open(address) {
                    Ok(socket) => return Ok((DialStream::Icmp(socket), pinned)),
                    Err(error) => last_error = Some(error),
                }
            }
            Err(icmp_open_error(host, last_error))
        }
    }
}

/// Guard `host` through the one egress policy and refuse a destination with no vetted address.
///
/// Shared by every IP variant so the "an empty or failed DNS answer is refused rather than
/// re-resolved by the connect path" rule cannot drift between them.
/// `destination` is how the caller spells the target in a diagnostic — `"tcp host:port"`,
/// `"icmp host"`. It is passed in rather than derived from `port`, because deriving it would key on
/// ICMP's port-0 sentinel and then silently drop the port from a genuine TCP or UDP dial to port 0.
fn vetted_or_refuse(
    host: &str,
    port: u16,
    allow: &PrivateNetAllow,
    resolver: &dyn HostResolver,
    destination: &str,
) -> Result<Vec<SocketAddr>> {
    let pinned = guard_target_host_pinned(host, port, allow, resolver)?;
    if pinned.is_empty() {
        return Err(Error::Other(format!(
            "refusing to dial {destination} — DNS returned no vetted addresses"
        )));
    }
    Ok(pinned)
}

/// The wildcard bind address in `peer`'s family, so a connect never crosses address families.
fn unspecified_for(peer: SocketAddr) -> SocketAddr {
    match peer.ip() {
        IpAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
        IpAddr::V6(_) => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
    }
}

fn describe_dial_failure(error: Option<std::io::Error>) -> String {
    error
        .map(|error| error.to_string())
        .unwrap_or_else(|| "no vetted address was connectable".to_string())
}

/// Turn a raw-ICMP open failure into a diagnosable refusal.
///
/// A missing privilege is a **deployment fact**, so the message names the capability rather than
/// repeating the kernel's "permission denied" — an operator can act on the former and not on the
/// latter. It is also stated as a refusal at construction, because that is the guarantee: nothing
/// was addressed and no datagram reached the destination.
fn icmp_open_error(host: &str, error: Option<std::io::Error>) -> Error {
    let Some(error) = error else {
        return Error::Other(format!(
            "refusing to dial icmp {host} — no vetted address could be opened"
        ));
    };
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        return Error::Other(format!(
            "refusing to dial icmp {host} at construction: this process may not open a raw ICMP \
             socket — it lacks CAP_NET_RAW (Linux; root on macOS). Grant the capability to the \
             process (e.g. `setcap cap_net_raw+ep`) or run the probe elsewhere. No datagram was \
             sent."
        ));
    }
    Error::Other(format!(
        "refusing to dial icmp {host} at construction: raw ICMP socket: {error}. No datagram was \
         sent."
    ))
}

/// Compatibility wrapper for callers that still need the old all-or-nothing shape.
pub async fn dial(target: &DialTarget, allow_private: bool) -> Result<DialStream> {
    dial_scoped(target, &PrivateNetAllow::from_legacy_bool(allow_private)).await
}

fn guard_target_host_pinned(
    host: &str,
    port: u16,
    allow: &PrivateNetAllow,
    resolver: &dyn HostResolver,
) -> Result<Vec<SocketAddr>> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        block_if(ip, host, allow)?;
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    let lower = host.to_ascii_lowercase();
    if is_internal_hostname(&lower) && !allow.allows_host(host) {
        return Err(Error::Other(format!(
            "refusing to dial internal host {host}"
        )));
    }
    let addresses = resolver
        .resolve(host, port)
        .map_err(|e| Error::Other(format!("resolving {host}:{port}: {e}")))?;
    let mut pinned = Vec::with_capacity(addresses.len());
    for address in addresses {
        block_if(address, host, allow)?;
        pinned.push(SocketAddr::new(address, port));
    }
    Ok(pinned)
}

/// Whether a guarded destination names or resolves to a private address.
///
/// Callers that audit an admitted private-network grant should use the same vetted address set
/// that was connected, rather than resolving the hostname a second time for classification.
pub fn destination_is_private(host: &str, pinned: &[SocketAddr]) -> bool {
    is_internal_hostname(&host.to_ascii_lowercase())
        || pinned.iter().any(|address| is_blocked_ip(address.ip()))
}

fn is_internal_hostname(lower: &str) -> bool {
    lower == "localhost"
        || lower.ends_with(".localhost")
        || lower == "metadata.google.internal"
        || lower.ends_with(".internal")
}

/// Match `host` against a list of host patterns: an exact name, a `*.suffix` wildcard that requires
/// a real label boundary, or `*`.
///
/// `pub(crate)` since C-459 so [`secret_scope`](crate::secret_scope) matches a secret's destination
/// scope with the identical vocabulary a private-net grant uses. A second implementation of "what
/// does `*.example.com` mean" is how two allow-lists come to disagree about one hostname.
pub(crate) fn host_matches(patterns: &[String], host: &str) -> bool {
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

    struct RebindingResolver {
        calls: std::sync::atomic::AtomicUsize,
        first: IpAddr,
        later: IpAddr,
    }

    impl HostResolver for RebindingResolver {
        fn resolve(&self, _host: &str, _port: u16) -> std::io::Result<Vec<IpAddr>> {
            let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(vec![if call == 0 { self.first } else { self.later }])
        }
    }

    /// C-257: the raw TCP path must consume the address authorized by the guard. Before the fix it
    /// called `TcpStream::connect((host, port))`, which performed a second DNS lookup and could take
    /// `later`. The fixed path calls the resolver once and connects directly to `first`.
    #[tokio::test]
    async fn pinned_dial_never_re_resolves_after_authorization() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let resolver = RebindingResolver {
            calls: std::sync::atomic::AtomicUsize::new(0),
            first: "127.0.0.1".parse().unwrap(),
            later: "169.254.169.254".parse().unwrap(),
        };
        let accept = tokio::spawn(async move { listener.accept().await.unwrap() });
        let target = DialTarget::Tcp {
            host: "rebind.test".to_string(),
            port,
        };
        let allow = PrivateNetAllow::from_hosts(["rebind.test".to_string()]);

        let (_stream, pinned) = dial_scoped_pinned_with_resolver(&target, &allow, &resolver)
            .await
            .expect("the vetted loopback address is connected directly");
        assert_eq!(pinned, vec![SocketAddr::from(([127, 0, 0, 1], port))]);
        assert_eq!(
            resolver.calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the connect path must not ask DNS for the attacker's second answer"
        );
        accept.await.unwrap();
    }

    #[tokio::test]
    async fn pinned_dial_fails_closed_on_an_empty_dns_answer() {
        let target = DialTarget::Tcp {
            host: "empty.test".to_string(),
            port: 443,
        };
        let resolver = FixedResolver(Vec::new());
        let err = dial_scoped_pinned_with_resolver(&target, &PrivateNetAllow::None, &resolver)
            .await
            .err()
            .expect("an empty vetted set cannot fall back to connect-time DNS");
        assert!(err.to_string().contains("no vetted addresses"), "{err}");
    }

    // -- C-396: datagram and raw dial targets ---------------------------------------------------

    /// A raw-ICMP opener that never opens anything. It records the peer it was asked for and reports
    /// the `PermissionDenied` the kernel returns from `socket(AF_INET, SOCK_RAW, IPPROTO_ICMP)` when
    /// the process holds no `CAP_NET_RAW`. The refusal *wording* is produced by this module, not by
    /// the double, so the test observes the production message rather than one it wrote itself.
    #[derive(Default)]
    struct DeniedIcmp {
        peers: std::sync::Mutex<Vec<SocketAddr>>,
    }

    impl RawIcmpOpener for DeniedIcmp {
        fn open(&self, peer: SocketAddr) -> std::io::Result<IcmpSocket> {
            self.peers.lock().unwrap().push(peer);
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        }
    }

    impl DeniedIcmp {
        fn asked(&self) -> Vec<SocketAddr> {
            self.peers.lock().unwrap().clone()
        }
    }

    /// A loopback UDP echo responder. Hermetic — nothing leaves the host.
    async fn udp_echo_once() -> u16 {
        let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let port = socket.local_addr().unwrap().port();
        tokio::spawn(async move {
            let mut buf = [0u8; 64];
            if let Ok((n, from)) = socket.recv_from(&mut buf).await {
                let _ = socket.send_to(&buf[..n], from).await;
            }
        });
        port
    }

    /// C-396 (Acceptance 2): a UDP destination runs through the *same* resolution and range guard as
    /// TCP — no second guard. A hostname answering a private address is refused without a grant and
    /// admitted with a scoped one, and the admitted dial really carries datagrams.
    #[tokio::test]
    async fn udp_dial_refuses_a_private_answer_and_admits_it_under_a_scoped_grant() {
        let target = DialTarget::Udp {
            host: "probe.test".to_string(),
            port: 9,
        };
        let rfc1918 = FixedResolver(vec!["10.0.0.1".parse().unwrap()]);
        let err = dial_scoped_pinned_with_resolver(&target, &PrivateNetAllow::None, &rfc1918)
            .await
            .err()
            .expect("a UDP host resolving to a private address must be refused");
        assert!(
            err.to_string().contains("10.0.0.1") && err.to_string().contains("probe.test"),
            "the UDP refusal must name the blocked resolved address: {err}"
        );

        // A grant scoped to some *other* host does not admit it.
        let wrong = PrivateNetAllow::from_hosts(["other.test".to_string()]);
        assert!(
            dial_scoped_pinned_with_resolver(&target, &wrong, &rfc1918)
                .await
                .is_err(),
            "a grant for a different host must not admit this one"
        );

        // With a grant for this host, the same guarded path admits the destination and the socket
        // round-trips. Loopback is a blocked range too, so a working echo proves both halves.
        let port = udp_echo_once().await;
        let target = DialTarget::Udp {
            host: "probe.test".to_string(),
            port,
        };
        let grant = PrivateNetAllow::from_hosts(["probe.test".to_string()]);
        let loopback = FixedResolver(vec!["127.0.0.1".parse().unwrap()]);
        let (mut stream, pinned) = dial_scoped_pinned_with_resolver(&target, &grant, &loopback)
            .await
            .expect("a scoped private-net grant admits the same destination");
        assert_eq!(pinned, vec![SocketAddr::from(([127, 0, 0, 1], port))]);
        stream.write_all(b"ping").await.unwrap();
        assert_eq!(&stream.read(64).await.unwrap(), b"ping");
        stream.shutdown().await.unwrap();
    }

    /// C-396 (Acceptance 3): a raw-ICMP target is refused **at construction** when the process may
    /// not open a raw socket, and the error names the capability rather than saying "permission
    /// denied". A check that happened on the wire would already have contacted the destination — so
    /// the test also pins the ordering: no `DialStream` exists, hence no send path exists, and the
    /// egress guard refuses a blocked destination *before* the opener is consulted at all.
    #[tokio::test]
    async fn raw_icmp_is_refused_at_construction_when_the_capability_is_absent() {
        // A public IP literal: the egress guard admits it and never asks DNS, so the refusal under
        // test is the capability one. Nothing is sent and no name is resolved.
        let opener = DeniedIcmp::default();
        let target = DialTarget::Icmp {
            host: "93.184.216.34".to_string(),
        };
        let err = dial_scoped_pinned_with_icmp_opener(
            &target,
            &PrivateNetAllow::None,
            &SystemHostResolver,
            &opener,
        )
        .await
        .err()
        .expect("a process without the raw-socket capability must be refused at construction");
        assert!(
            err.to_string().contains("CAP_NET_RAW"),
            "the refusal must name the capability, not just report permission denied: {err}"
        );
        assert_eq!(
            opener.asked(),
            vec![SocketAddr::new("93.184.216.34".parse().unwrap(), 0)],
            "the socket is opened once, at construction, for the vetted peer"
        );

        // The same guard governs ICMP: a blocked destination is refused before a raw socket is ever
        // requested, so a privileged and an unprivileged process are refused identically.
        let opener = DeniedIcmp::default();
        let blocked = DialTarget::Icmp {
            host: "169.254.169.254".to_string(),
        };
        let err = dial_scoped_pinned_with_icmp_opener(
            &blocked,
            &PrivateNetAllow::None,
            &SystemHostResolver,
            &opener,
        )
        .await
        .err()
        .expect("cloud metadata must be refused for ICMP exactly as it is for TCP");
        assert!(err.to_string().contains("169.254.169.254"), "{err}");
        assert!(
            opener.asked().is_empty(),
            "the egress guard must refuse before any raw socket is opened"
        );

        // ...and a scoped grant admits it as far as the capability check, which is the only thing
        // left to refuse it.
        let opener = DeniedIcmp::default();
        let grant = PrivateNetAllow::from_hosts(["169.254.169.254".to_string()]);
        let err =
            dial_scoped_pinned_with_icmp_opener(&blocked, &grant, &SystemHostResolver, &opener)
                .await
                .err()
                .expect("the capability is still required for a granted destination");
        assert!(err.to_string().contains("CAP_NET_RAW"), "{err}");
        assert_eq!(opener.asked().len(), 1);
    }

    /// C-396 (Acceptance 4): the `dial_scoped_pinned` property extended to the datagram variants —
    /// the address the guard approved is the address dialled. DNS is consulted exactly once, and the
    /// UDP socket is *connected* to that answer, so the kernel itself refuses to carry a datagram
    /// anywhere else. The rebinding resolver's second, internal answer never reaches a socket.
    #[tokio::test]
    async fn datagram_dials_pin_the_address_the_guard_approved() {
        let allow = PrivateNetAllow::from_hosts(["rebind.test".to_string()]);
        let vetted = SocketAddr::from(([127, 0, 0, 1], 9));

        let resolver = RebindingResolver {
            calls: std::sync::atomic::AtomicUsize::new(0),
            first: "127.0.0.1".parse().unwrap(),
            later: "169.254.169.254".parse().unwrap(),
        };
        let target = DialTarget::Udp {
            host: "rebind.test".to_string(),
            port: 9,
        };
        let (stream, pinned) = dial_scoped_pinned_with_resolver(&target, &allow, &resolver)
            .await
            .expect("the vetted loopback answer is connected directly");
        assert_eq!(pinned, vec![vetted]);
        assert_eq!(
            resolver.calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the UDP connect path must not ask DNS for the attacker's second answer"
        );
        match stream {
            DialStream::Udp(socket) => assert_eq!(
                socket.peer_addr().unwrap(),
                vetted,
                "the datagram socket must be connected to the vetted peer"
            ),
            _ => panic!("a UDP target must yield a UDP stream"),
        }

        // ICMP has no port, so it pins the vetted address at port 0 — observed at the seam that
        // opens the socket, which is the only place a raw datagram could be addressed.
        let resolver = RebindingResolver {
            calls: std::sync::atomic::AtomicUsize::new(0),
            first: "127.0.0.1".parse().unwrap(),
            later: "169.254.169.254".parse().unwrap(),
        };
        let opener = DeniedIcmp::default();
        let target = DialTarget::Icmp {
            host: "rebind.test".to_string(),
        };
        let _ = dial_scoped_pinned_with_icmp_opener(&target, &allow, &resolver, &opener).await;
        assert_eq!(opener.asked(), vec![SocketAddr::from(([127, 0, 0, 1], 0))]);
        assert_eq!(
            resolver.calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the ICMP path must not re-resolve after authorization"
        );
    }

    /// The one test that runs the real `SystemRawIcmp::open` — the `unsafe` block and its
    /// close-on-exec decision have no coverage otherwise.
    ///
    /// It asserts what holds on *whichever* branch the host takes, because whether this process may
    /// open a raw socket is a property of the deployment and not of the code:
    ///
    /// - **Privileged** (CI running as root, or `unshare -r -n`): the descriptor must come back
    ///   close-on-exec. That is the property the atomic `SOCK_CLOEXEC` exists to guarantee, and it
    ///   is the half a portable test *can* pin once the branch is reachable.
    /// - **Unprivileged** (the ordinary case, including CI): the refusal must be `PermissionDenied`
    ///   and must map to a message naming `CAP_NET_RAW`.
    ///
    /// It cannot pin the *absence of the window* on Apple, because there is none to pin — see
    /// `raw_socket_type`. Loopback is used so `connect` needs no route and nothing leaves the host.
    #[tokio::test]
    async fn the_real_raw_icmp_opener_is_close_on_exec_or_refuses_by_capability() {
        let peer = SocketAddr::from(([127, 0, 0, 1], 0));
        match SystemRawIcmp.open(peer) {
            Ok(socket) => {
                assert_eq!(socket.peer(), peer);
                #[cfg(unix)]
                {
                    use std::os::fd::AsRawFd;
                    // SAFETY: reading the descriptor flags of a socket this scope owns.
                    let flags = unsafe { libc::fcntl(socket.socket.as_raw_fd(), libc::F_GETFD) };
                    assert!(flags >= 0, "F_GETFD failed on the raw ICMP socket");
                    assert_eq!(
                        flags & libc::FD_CLOEXEC,
                        libc::FD_CLOEXEC,
                        "a raw ICMP socket must be close-on-exec or a concurrent spawn inherits it"
                    );
                }
            }
            Err(error) => {
                let denied = error.kind() == std::io::ErrorKind::PermissionDenied;
                let mapped = icmp_open_error("127.0.0.1", Some(error)).to_string();
                assert!(
                    mapped.contains("at construction") && mapped.contains("No datagram was sent"),
                    "every real open failure must read as a refusal before the wire: {mapped}"
                );
                if denied {
                    assert!(
                        mapped.contains("CAP_NET_RAW"),
                        "an EPERM from the real opener must name the capability: {mapped}"
                    );
                }
            }
        }
    }

    /// The close-on-exec **window** cannot be observed after the fact: once `open` returns, the
    /// descriptor is close-on-exec either way, and the difference between the atomic form and a
    /// following `fcntl` is visible only to a `fork` that interleaved between the two. Racing a
    /// spawn to catch it would be flaky, and the runtime-behaviour test above therefore pins the
    /// end state, not the atomicity.
    ///
    /// So the atomic form is pinned where it *is* deterministic — at the source. The one
    /// `libc::socket` call in this module must take its type from `raw_socket_type`, which is what
    /// folds in `SOCK_CLOEXEC`; a future edit back to a bare `SOCK_RAW` reopens the window silently
    /// and this is the only thing that would notice.
    #[cfg(unix)]
    #[test]
    fn the_raw_socket_is_created_close_on_exec_atomically() {
        // Assembled at runtime so this test's own source does not match the needle it searches for.
        let needle = format!("{}{}", "libc::", "socket(");
        let call_sites: Vec<&str> = include_str!("net.rs")
            .lines()
            .filter(|line| line.contains(&needle))
            .collect();
        assert_eq!(
            call_sites.len(),
            1,
            "expected exactly one raw socket creation in this module: {call_sites:?}"
        );
        assert!(
            call_sites[0].contains("raw_socket_type()"),
            "the raw socket must request its close-on-exec type atomically at creation, not set it \
             afterwards — see `raw_socket_type`: {}",
            call_sites[0]
        );
    }

    /// `SystemRawIcmp::open` is a sync method on a published trait, so being called without a
    /// runtime is a caller error to *return*, not to panic on.
    #[test]
    fn the_real_raw_icmp_opener_reports_a_missing_runtime_instead_of_panicking() {
        let error = SystemRawIcmp
            .open(SocketAddr::from(([127, 0, 0, 1], 0)))
            .expect_err("no tokio runtime is running in a plain #[test]");
        assert!(error.to_string().contains("tokio runtime"), "{error}");
    }

    /// A datagram too large for the read buffer must fail loudly. The kernel has already discarded
    /// the tail, so a caller validating a reply would otherwise be validating a beheaded one.
    #[tokio::test]
    async fn an_oversized_datagram_is_an_error_rather_than_a_silent_truncation() {
        let port = udp_echo_once().await;
        let peer = SocketAddr::from(([127, 0, 0, 1], port));
        let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        socket.connect(peer).await.unwrap();
        let mut stream = DialStream::Icmp(IcmpSocket { socket, peer });

        stream.write_all(b"0123456789").await.unwrap();
        let err = stream
            .read(4)
            .await
            .expect_err("a 10-byte datagram does not fit in a 4-byte read");
        assert!(
            err.to_string().contains("exceeds the 4-byte read buffer"),
            "{err}"
        );

        // A datagram that exactly fills the buffer is not truncated and must still be delivered.
        let port = udp_echo_once().await;
        let peer = SocketAddr::from(([127, 0, 0, 1], port));
        let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        socket.connect(peer).await.unwrap();
        let mut stream = DialStream::Udp(socket);
        stream.write_all(b"1234").await.unwrap();
        assert_eq!(&stream.read(4).await.unwrap(), b"1234");
    }

    /// The refusal text names the port for a port-bearing variant, including port 0 — it must not be
    /// derived from ICMP's port-0 sentinel.
    #[tokio::test]
    async fn the_refusal_names_the_port_for_port_bearing_variants() {
        let resolver = FixedResolver(Vec::new());
        for (target, expected) in [
            (
                DialTarget::Tcp {
                    host: "empty.test".to_string(),
                    port: 0,
                },
                "tcp empty.test:0",
            ),
            (
                DialTarget::Udp {
                    host: "empty.test".to_string(),
                    port: 0,
                },
                "udp empty.test:0",
            ),
            (
                DialTarget::Icmp {
                    host: "empty.test".to_string(),
                },
                "icmp empty.test",
            ),
        ] {
            let err = dial_scoped_pinned_with_icmp_opener(
                &target,
                &PrivateNetAllow::None,
                &resolver,
                &DeniedIcmp::default(),
            )
            .await
            .err()
            .expect("an empty vetted set is refused");
            assert!(err.to_string().contains(expected), "{err}");
        }
    }

    /// An empty DNS answer must not fall back to connect-time resolution on the new variants either.
    #[tokio::test]
    async fn datagram_dials_fail_closed_on_an_empty_dns_answer() {
        let resolver = FixedResolver(Vec::new());
        for target in [
            DialTarget::Udp {
                host: "empty.test".to_string(),
                port: 53,
            },
            DialTarget::Icmp {
                host: "empty.test".to_string(),
            },
        ] {
            let err = dial_scoped_pinned_with_icmp_opener(
                &target,
                &PrivateNetAllow::None,
                &resolver,
                &DeniedIcmp::default(),
            )
            .await
            .err()
            .expect("an empty vetted set cannot fall back to connect-time DNS");
            assert!(err.to_string().contains("no vetted addresses"), "{err}");
        }
    }

    /// The `DialStream` shim over a raw ICMP socket has datagram, not stream, semantics: one
    /// `write_all` is one datagram and one `read` is one datagram. This exercises the shim over a
    /// loopback UDP pair — it says nothing about raw-socket privilege, which no unprivileged test
    /// can exercise.
    #[tokio::test]
    async fn the_icmp_stream_shim_carries_one_datagram_per_call() {
        let port = udp_echo_once().await;
        let peer = SocketAddr::from(([127, 0, 0, 1], port));
        let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        socket.connect(peer).await.unwrap();
        let mut stream = DialStream::Icmp(IcmpSocket { socket, peer });
        stream.write_all(b"echo").await.unwrap();
        assert_eq!(&stream.read(64).await.unwrap(), b"echo");
        stream.shutdown().await.unwrap();
    }
}
