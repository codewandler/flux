//! Deterministic URL/redirect normalization corpus for the shared egress guard.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use flux_system::net::{guard_url_scoped_pinned_with_resolver, HostResolver, PrivateNetAllow};

struct FixedResolver(IpAddr);

impl HostResolver for FixedResolver {
    fn resolve(&self, _host: &str, _port: u16) -> std::io::Result<Vec<IpAddr>> {
        Ok(vec![self.0])
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut value = self.0;
        value ^= value >> 12;
        value ^= value << 25;
        value ^= value >> 27;
        self.0 = value;
        value.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

fn cases() -> usize {
    std::env::var("FLUX_ADVERSARIAL_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(128)
        .clamp(1, 8192)
}

fn blocked_ip(rng: &mut Rng) -> IpAddr {
    let a = rng.next() as u8;
    let b = rng.next() as u8;
    match rng.next() % 10 {
        0 => IpAddr::V4(Ipv4Addr::new(127, a, b, 1)),
        1 => IpAddr::V4(Ipv4Addr::new(10, a, b, 2)),
        2 => IpAddr::V4(Ipv4Addr::new(172, 16 + a % 16, b, 3)),
        3 => IpAddr::V4(Ipv4Addr::new(192, 168, a, b)),
        4 => IpAddr::V4(Ipv4Addr::new(169, 254, a, b)),
        5 => IpAddr::V4(Ipv4Addr::new(100, 64 + a % 64, b, 4)),
        6 => IpAddr::V4(Ipv4Addr::new(0, a, b, 5)),
        7 => IpAddr::V6(Ipv6Addr::LOCALHOST),
        8 => IpAddr::V6(Ipv6Addr::new(0xfc00 | a as u16, b as u16, 0, 0, 0, 0, 0, 1)),
        _ => IpAddr::V6(Ipv4Addr::new(127, a, b, 6).to_ipv6_mapped()),
    }
}

fn ip_url(ip: IpAddr, case: usize) -> String {
    match ip {
        IpAddr::V4(ip) => match case % 3 {
            0 => format!("http://{ip}/artifact"),
            1 => format!("http://public.example@{ip}:8080/artifact"),
            _ => format!("//{ip}/artifact"),
        },
        IpAddr::V6(ip) => match case % 3 {
            0 => format!("http://[{ip}]/artifact"),
            1 => format!("http://public.example@[{ip}]:8080/artifact"),
            _ => format!("//[{ip}]/artifact"),
        },
    }
}

#[test]
fn generated_redirect_targets_never_normalize_around_private_net_policy() {
    let base = url::Url::parse("https://public.example/releases/current/index.json").unwrap();
    let deny = PrivateNetAllow::None;

    // Regression seeds cover the cloud-metadata, loopback, IPv4-mapped, scheme-relative redirect,
    // and userinfo-confusion shapes that have historically been easiest to mishandle.
    let corpus = [
        "http://169.254.169.254/latest/meta-data/",
        "http://127.0.0.1/admin",
        "http://[::ffff:10.0.0.1]/",
        "//[::1]/redirected",
        "http://public.example@10.0.0.1/looks-public",
    ];
    for target in corpus {
        let joined = base.join(target).unwrap();
        assert!(
            guard_url_scoped_pinned_with_resolver(
                joined.as_str(),
                &deny,
                &FixedResolver("203.0.113.8".parse().unwrap()),
            )
            .is_err(),
            "known-bad target was admitted after normalization: {joined}"
        );
    }

    let mut rng = Rng(0xC264_0A11_D1E5_0001);
    for case in 0..cases() {
        let ip = blocked_ip(&mut rng);
        let target = ip_url(ip, case);
        let joined = base.join(&target).unwrap_or_else(|error| {
            panic!("case {case}: generated redirect {target:?} did not parse: {error}")
        });
        assert!(
            guard_url_scoped_pinned_with_resolver(
                joined.as_str(),
                &deny,
                &FixedResolver("203.0.113.9".parse().unwrap()),
            )
            .is_err(),
            "case {case}: blocked address {ip} escaped through {joined}"
        );

        // Exercise the domain-resolution half with the same independently generated blocked IP.
        let domain = format!("https://case-{case}.example/redirected");
        assert!(
            guard_url_scoped_pinned_with_resolver(&domain, &deny, &FixedResolver(ip)).is_err(),
            "case {case}: DNS answer {ip} was admitted for {domain}"
        );
    }
}
