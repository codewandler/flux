//! Where a `connector` channel's manifest is read from (D-216).
//!
//! `~/.flux/connectors/<connector>.connector.toml`, resolved through `flux_system::System` — beside
//! `~/.flux/flows`, which is already how the same connector's `.flux` module reaches a host, so an
//! installed connector is one directory pair rather than two mechanisms.
//!
//! # Why this is a test binary of its own
//!
//! Every other test in `connector_channel.rs` passes the `manifest` override, because each is about
//! some *other* defect and an override keeps the fixture beside the assertion. That leaves the
//! default resolution — the path every real deployment takes — observed by nothing. This file is
//! that observation.
//!
//! It repoints `HOME`, which is process-global state shared by every test thread in a binary. One
//! test in one binary is the only arrangement where that cannot race; a lock would work too, but
//! only for as long as everyone who adds a test to the file remembers it exists.

use std::path::{Path, PathBuf};

use flux_channels::build_channels;
use flux_lang::program::ChannelDecl;
use serde_json::{json, Value};

/// A checked-in fixture manifest, as an absolute path.
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/connectors")
        .join(name)
}

/// A `connector` channel named `support` carrying **no `manifest` override** — the whole point.
fn decl(extra: Value) -> ChannelDecl {
    let mut settings = json!({
        "connector": "acme",
        "binding": "events-api",
        "addr": "127.0.0.1:0",
        "path": "/acme",
    });
    let (Value::Object(base), Value::Object(extra)) = (&mut settings, extra) else {
        unreachable!("both are objects")
    };
    base.extend(extra);
    ChannelDecl {
        name: "support".to_string(),
        kind: "connector".to_string(),
        settings,
    }
}

/// The default resolution, end to end: not installed → a refusal naming the path it looked in;
/// installed → the same declaration loads, with no override anywhere.
///
/// The two halves are one test on purpose. Asserting only the refusal would pass against an arm that
/// can never find a manifest; asserting only the load would pass against one whose "not installed"
/// error names nothing an operator can act on.
#[test]
fn the_default_manifest_is_read_from_the_flux_connectors_home() {
    let home = std::env::temp_dir().join(format!("flux-d216-home-{}", std::process::id()));
    let connectors = home.join(".flux").join("connectors");
    std::fs::create_dir_all(&connectors).expect("create the scratch connectors home");
    std::env::set_var("HOME", &home);

    // ── 1. Nothing installed ──────────────────────────────────────────────────────────────────
    // `Box<dyn Channel>` is not `Debug`, so the `Ok` arm is spelled out rather than `expect_err`.
    let err = match build_channels(&[decl(json!({}))]) {
        Ok(_) => panic!("a connector this host has not installed must not load"),
        Err(e) => e.to_string(),
    };
    assert!(
        err.contains("no manifest for connector `acme`"),
        "the refusal reads as `this host has not installed that connector`: {err}"
    );
    assert!(
        err.contains(
            connectors
                .join("acme.connector.toml")
                .to_str()
                .expect("UTF-8")
        ),
        "the refusal names the exact path it looked in, so an operator can act on it: {err}"
    );

    // ── 2. Installed under the connector home ─────────────────────────────────────────────────
    std::fs::copy(
        fixture("acme.connector.toml"),
        connectors.join("acme.connector.toml"),
    )
    .expect("install the connector manifest");

    let built = build_channels(&[decl(json!({}))])
        .expect("the installed manifest resolves with no `manifest` override");
    assert_eq!(built.len(), 1);
    assert_eq!(built[0].name(), "support");
    assert_eq!(built[0].required_tool(), Some("acme-messages-post"));

    // ── 3. A named service selects a different stem in the same directory ─────────────────────
    // `<connector>-<service>.connector.toml`, the producing repository's own naming rule. Asserted
    // here rather than as a unit test on the filename because the filename is only a claim until
    // something opens it.
    std::fs::copy(
        fixture("widget-hooks.connector.toml"),
        connectors.join("widget-hooks.connector.toml"),
    )
    .expect("install the second connector's manifest");

    let built = build_channels(&[decl(json!({
        "connector": "widget",
        "service": "hooks",
        "binding": "hooks",
    }))])
    .expect("a named service resolves to `<connector>-<service>.connector.toml`");
    assert_eq!(built.len(), 1);

    std::fs::remove_dir_all(&home).ok();
}
