//! The wire contract, pinned.
//!
//! These tests exist because the plugin protocol is versioned **independently of flux** (C-143):
//! nothing else stops a field rename or a semantic change from silently orphaning every plugin
//! binary already in the wild. Rust signatures are not the contract — the JSON on the pipe is —
//! so the assertions here are all about bytes.
//!
//! Three guarantees:
//!
//! 1. **Backward compatibility.** JSON captured from the shipped `flux.plugin.v1` wire still
//!    deserializes. A plugin built against protocol 1.0 keeps working against this host.
//! 2. **Surface visibility.** A maximal instance — every field set, built with exhaustive struct
//!    literals so a *new* field fails to compile here — serializes to a checked-in golden. You
//!    cannot change the wire without this test making you look at it and decide whether
//!    [`PROTOCOL`] and the crate's major version must move.
//! 3. **Marker enforcement.** [`check_protocol`] accepts its own marker and rejects everything
//!    else with a message naming both sides.

#[path = "support/golden_mode.rs"]
mod golden_mode;

use flux_plugin_protocol::*;
use golden_mode::Mode;
use serde_json::{json, Value};

fn golden(name: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read golden {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse golden {}: {e}", path.display()))
}

/// Set `FLUX_UPDATE_GOLDEN=1` to rewrite a golden after a *deliberate* wire change. That run writes
/// the file and then fails on purpose (C-326) — re-run with the variable unset to verify.
fn assert_golden(name: &str, actual: &Value) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    if golden_mode::mode() == Mode::Rewrite {
        let mut text = serde_json::to_string_pretty(actual).unwrap();
        text.push('\n');
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        golden_mode::rewrote(&path);
    }
    let expected = golden(name);
    assert_eq!(
        actual,
        &expected,
        "the plugin wire surface changed ({name}).\n\
         Every plugin binary in the wild was built against the old shape, so decide deliberately:\n\
           - additive + `serde` default/skip => compatible; re-record with `FLUX_UPDATE_GOLDEN=1` \
             and bump the MINOR of codewandler-flux-plugin-protocol\n\
           - a rename, a removal, or a changed meaning => breaking; bump `PROTOCOL` and the crate's \
             MAJOR, and expect to reship the plugin pack\n\
         See docs/designs/plugin-protocol-decoupling.md."
    );
}

/// A manifest with every field populated. Deliberately built with exhaustive struct literals and
/// **no** `..Default::default()`: adding a wire field breaks this file's compilation, which is the
/// point — the surface cannot grow unnoticed.
fn maximal_manifest() -> PluginManifest {
    PluginManifest {
        name: "fixture".into(),
        version: "9.9.9".into(),
        operations: vec![OperationSpec {
            public_name: Some("fixture.public".into()),
            name: "fixture.op".into(),
            description: "every field set".into(),
            input_schema: json!({"type": "object"}),
            output_schema: Some(json!({"type": "string"})),
            effects: vec![flux_spec::Effect::Read, flux_spec::Effect::Network],
            risk: Some(flux_spec::Risk::High),
            idempotency: Some(flux_spec::Idempotency::Idempotent),
            staging: flux_spec::StagingDisposition::Gather,
            secret_purposes: vec!["api".into()],
            process: vec!["kubectl get".into()],
            group: Some("fixture-group".into()),
            semantic_effects: vec![flux_spec::FlowEffect::Money],
            internal: true,
            redact_fields: vec!["token".into()],
            platform: PlatformSourcing::Activation,
            reaches: VendorReach::Host("api.fixture.invalid".into()),
        }],
        auth: vec![AuthMethod {
            purpose: "api".into(),
            env: vec!["FIXTURE_TOKEN".into()],
            description: "fixture auth".into(),
            scheme: AuthScheme::Header {
                name: "PRIVATE-TOKEN".into(),
            },
            user_env: vec!["FIXTURE_USER".into()],
            oauth2: Some(OAuth2Spec {
                endpoint: "https://example.invalid".into(),
                authorize_path: "/oauth/authorize".into(),
                token_path: "/oauth/token".into(),
                client_id: "fixture-client".into(),
                scopes: vec!["read".into()],
                grants: vec![OAuthGrant::AuthorizationCode],
                redirect: Some(OAuthRedirect {
                    port: 7777,
                    path: "/callback".into(),
                }),
            }),
        }],
        datasources: vec![flux_datasource::Declaration {
            name: "fixture-ds".into(),
            entity: "ticket".into(),
            description: Some("fixture datasource".into()),
            capabilities: vec!["search".into()],
            entity_schema: None,
        }],
        groups: vec![flux_evidence::ToolGroup {
            name: "fixture-group".into(),
            description: "fixture group".into(),
            tools: vec!["fixture.op".into()],
            surface_when: vec![flux_evidence::SignalMatch {
                kind: flux_evidence::KIND_TURN_INTENT.into(),
                signal: Some("fixture".into()),
            }],
        }],
        endpoints: vec![EndpointSpec {
            name: "api".into(),
            env: vec!["FIXTURE_URL".into()],
            http_hosts: vec!["example.invalid".into()],
            description: "fixture endpoint".into(),
            default: Some("https://example.invalid".into()),
            template: Some("https://{region}.example.invalid".into()),
        }],
        config: vec![ConfigSpec {
            name: "region".into(),
            env: vec!["FIXTURE_REGION".into()],
            description: "fixture config".into(),
        }],
        discovers: vec!["prometheus".into()],
        capabilities: PluginCapabilities {
            process: vec!["kubectl get".into()],
            secrets: vec!["FIXTURE_TOKEN".into()],
            http: true,
            websocket: true,
            http_hosts: vec!["example.invalid".into()],
            private_hosts: vec!["10.0.0.1".into()],
            conn: vec!["tcp:*:5432".into()],
            blob: true,
            discover: true,
            credential: true,
            fs: vec![FsReadScope {
                path: "/etc/fixture".into(),
                secret: true,
            }],
        },
    }
}

/// Same intent as [`maximal_manifest`] for the frame envelope.
fn maximal_frame() -> Frame {
    Frame {
        protocol: PROTOCOL.into(),
        id: "r1".into(),
        kind: FrameKind::Response,
        command: "call".into(),
        payload: json!({"operation": "fixture.op"}),
        ok: true,
        result: json!({"value": 1}),
        error: Some("fixture error".into()),
    }
}

#[test]
fn frame_wire_surface_is_pinned() {
    assert_golden(
        "frame.json",
        &serde_json::to_value(maximal_frame()).unwrap(),
    );
}

#[test]
fn manifest_wire_surface_is_pinned() {
    assert_golden(
        "manifest.json",
        &serde_json::to_value(maximal_manifest()).unwrap(),
    );
}

#[test]
fn websocket_capability_round_trips_as_an_explicit_deny_by_default_wire_grant() {
    let decoded: PluginCapabilities = serde_json::from_value(json!({"websocket": true}))
        .expect("additive websocket capability should deserialize");
    let wire = serde_json::to_value(decoded).expect("capability should serialize");
    assert_eq!(wire.get("websocket"), Some(&json!(true)));

    let denied = serde_json::to_value(PluginCapabilities::default())
        .expect("default capabilities should serialize");
    assert_eq!(denied.get("websocket"), Some(&json!(false)));
}

#[test]
fn the_pinned_wire_round_trips() {
    for name in ["frame.json", "manifest.json"] {
        let value = golden(name);
        let reserialized = if name == "frame.json" {
            serde_json::to_value(serde_json::from_value::<Frame>(value.clone()).unwrap()).unwrap()
        } else {
            serde_json::to_value(serde_json::from_value::<PluginManifest>(value.clone()).unwrap())
                .unwrap()
        };
        assert_eq!(reserialized, value, "{name} did not survive a round trip");
    }
}

/// A minimal manifest — the shape a hand-rolled plugin in another language actually emits — must
/// keep loading. Every optional field carries a `serde` default precisely so this holds, and this
/// test is what keeps that true.
#[test]
fn a_minimal_foreign_manifest_still_loads() {
    let manifest: PluginManifest = serde_json::from_value(json!({
        "name": "minimal",
        "operations": [{
            "name": "minimal.op",
            "description": "no optional fields at all",
            "input_schema": {"type": "object"}
        }]
    }))
    .expect("a manifest with only required fields must load");

    assert_eq!(manifest.name, "minimal");
    assert_eq!(manifest.operations.len(), 1);
    assert!(manifest.capabilities.process.is_empty());
    assert!(!manifest.capabilities.http);
    assert!(manifest.operations[0].semantic_effects.is_empty());
}

/// A frame as emitted by a plugin built against protocol 1.0 — no more, no less.
#[test]
fn a_v1_frame_from_an_older_plugin_still_loads() {
    let frame: Frame = serde_json::from_value(json!({
        "protocol": "flux.plugin.v1",
        "id": "r1",
        "type": "response",
        "command": "manifest",
        "ok": true,
        "result": {"name": "old", "operations": []}
    }))
    .expect("a 1.0-shaped frame must load");

    assert_eq!(frame.protocol, PROTOCOL);
    assert!(frame.ok);
    assert!(frame.error.is_none());
    check_protocol(&frame.protocol).expect("the shipped marker must be accepted");
}

#[test]
fn check_protocol_rejects_a_foreign_marker_with_both_sides_named() {
    let err = check_protocol("flux.plugin.v2").expect_err("a different marker must be rejected");
    assert!(
        err.contains("flux.plugin.v2"),
        "names the plugin's marker: {err}"
    );
    assert!(err.contains(PROTOCOL), "names the host's marker: {err}");

    let empty = check_protocol("").expect_err("an absent marker must be rejected");
    assert!(empty.contains(PROTOCOL), "{empty}");
}
