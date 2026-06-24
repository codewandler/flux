//! Network egress guard — the single SSRF policy reused by every surface that fetches a URL
//! (`flux-browser`'s `web_fetch`, `flux-plugin`'s `http.do`).
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
}
