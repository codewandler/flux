//! The generic `kind = "connector"` arm (D-216).
//!
//! A connector manifest is a **published artifact**, and a published artifact can be edited after
//! publication. So every rule the producing repository enforces at compile time is enforced again
//! here, against the file actually on disk, and it is enforced in the decl-only `build_channels` —
//! **before any listener binds**. This suite asserts the refusals, not the request path.

use flux_channels::build_channels;
use flux_lang::program::ChannelDecl;
use serde_json::json;

/// A fixture manifest, as a path relative to the crate root (which is a test binary's cwd, and the
/// root a `flux_system::System` built from the environment is confined to).
fn fixture(name: &str) -> String {
    format!("tests/fixtures/connectors/{name}")
}

/// D-216's failing-first test.
///
/// A `webhook` binding that states no verification is refused at **load**. The producing repository
/// already refuses it — `VerificationScheme` is a tri-state and an unset one on a webhook binding is
/// a loader error there — and the whole point of this arm is that flux cannot be the bypass: the
/// refusal is reproduced against the bytes on disk.
///
/// The assertion is on the constructor's result and on the port, never on a request: an arm that
/// bound first and validated on the first delivery would have already exposed an unauthenticated
/// endpoint to the internet by the time it noticed.
#[test]
fn unverified_webhook_binding_is_refused_at_load() {
    // Reserve an ephemeral port and release it, so the declaration names a real, free address. If
    // anything in `build_channels` bound a listener, the rebind at the end of this test fails.
    let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve an ephemeral port");
    let addr = probe.local_addr().expect("the reserved address");
    drop(probe);

    let decl = ChannelDecl {
        name: "support".to_string(),
        kind: "connector".to_string(),
        settings: json!({
            "connector": "acme",
            "binding": "events-api",
            "manifest": fixture("acme-unverified.connector.toml"),
            "addr": addr.to_string(),
            "path": "/acme",
        }),
    };

    let err = match build_channels(std::slice::from_ref(&decl)) {
        Ok(_) => panic!("a webhook binding with no stated verification must not load"),
        Err(e) => e.to_string(),
    };
    assert!(
        err.contains("support"),
        "the error names the channel: {err}"
    );
    assert!(
        err.to_lowercase().contains("verification"),
        "the error names the missing verification: {err}"
    );

    // No port was bound before the refusal.
    std::net::TcpListener::bind(addr).expect("nothing was bound: the address is still free");
}
