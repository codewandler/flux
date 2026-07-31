//! The generic `kind = "connector"` arm (D-216).
//!
//! A connector manifest is a **published artifact**, and a published artifact can be edited after
//! publication. So every rule the producing repository enforces at compile time is enforced again
//! here, against the file actually on disk, and it is enforced in the decl-only `build_channels` —
//! **before any listener binds**. This suite is therefore mostly a table of refusals.
//!
//! # How "no port is bound" is asserted, and why it does not race
//!
//! Two independent, deterministic guards, neither of which reserves-then-releases an ephemeral port
//! (which would race the OS reusing it):
//!
//! 1. **The test holds the listener itself** for its whole body. Nothing else can bind that address
//!    — tokio sets no `SO_REUSEPORT` — so a constructor that tried would fail with an
//!    address-in-use error, and [`assert_refused_for`] fails on exactly that swap.
//! 2. **The load tests are plain `#[test]`s, not `#[tokio::test]`s.** `Channel::start` binds through
//!    `tokio::net::TcpListener`, which panics without a reactor. A constructor that bound anything
//!    would blow up here rather than pass quietly.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use flux_app::{App, JourneyRun};
use flux_channels::{build_channels, serve, ConnectorChannel, Deliverer};
use flux_lang::program::{ChannelDecl, Module};
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt; // for `oneshot`

// ─────────────────────────────────────────────────────────────────────────────────────────────────
// Scaffolding
// ─────────────────────────────────────────────────────────────────────────────────────────────────

/// A checked-in fixture manifest, as an absolute path.
///
/// Absolute, not crate-root-relative: the adapter roots a `flux_system::Workspace` at the manifest's
/// **own directory** and confines the read to it, so an absolute path is both the honest spelling
/// and independent of a test binary's cwd.
fn fixture(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/connectors")
        .join(name)
        .to_str()
        .expect("the fixture path is UTF-8")
        .to_string()
}

/// A scratch directory that removes itself, for the mutated manifests the refusal table needs.
///
/// Each defect below is a one-line edit to an otherwise-correct file. Writing each as its own
/// checked-in fixture would put the defect in a different file from the assertion about it — the
/// wrong trade for a suite whose whole job is "this specific defect is refused". The two *shapes*
/// the suite depends on structurally (a servable binding, and a second differently-shaped connector)
/// are checked in; the defects are generated.
struct Scratch(PathBuf);

impl Scratch {
    /// Write `body` as connector `acme`'s manifest and return its absolute path.
    fn manifest(body: &str) -> (Self, String) {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "flux-d216-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("create the scratch dir");
        let path = dir.join("acme.connector.toml");
        std::fs::write(&path, body).expect("write the scratch manifest");
        let path = path.to_str().expect("UTF-8").to_string();
        (Self(dir), path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A manifest for connector `acme` carrying exactly the `[[channels]]` block given.
fn manifest_with(binding: &str) -> String {
    format!(
        r#"generator = "flux-connectors 0.6.0"
connector = "acme"
vendor = "Acme"
base_url = "https://api.acme.example"
module = "acme.flux"
operations = ["acme-messages-post"]

[[events]]
name = "mention"

[[events]]
name = "reaction"

{binding}
"#
    )
}

/// A webhook binding whose verification is `hmac`, with `body` splicing in the parameters under test.
fn hmac_binding(body: &str) -> String {
    format!(
        r#"[[channels]]
name = "events-api"
transport = "webhook"
events = ["mention"]

[channels.verification]
kind = "hmac"
verified = true

[channels.verification.hmac]
algorithm = "sha256"
encoding = "hex"
header = "X-Acme-Signature"
secret = "acme.signing_secret"
{body}"#
    )
}

/// A `connector` channel declaration named `support`, on loopback, with `extra` merged over it.
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

/// The same declaration with the binding's signing credential mapped, so a test about some *other*
/// defect is not answered by the credential refusal first.
fn credentialled(manifest: &str) -> Value {
    let mut credentials = BTreeMap::new();
    credentials.insert("acme.signing_secret".to_string(), "s3cret".to_string());
    json!({ "manifest": manifest, "credentials": credentials })
}

/// Build one channel and return the error it must have produced.
fn refusal(decl: ChannelDecl) -> String {
    match build_channels(std::slice::from_ref(&decl)) {
        Ok(_) => panic!("expected a load error, but the channel was constructed"),
        Err(e) => {
            let text = e.to_string();
            assert!(
                text.contains("support") || text.contains("builds"),
                "every refusal names the channel: {text}"
            );
            text
        }
    }
}

/// Build every channel a **program** declares and return the error it must have produced.
///
/// The long way round on purpose: it is the only spelling that proves the settings this arm reads
/// are the settings an operator can actually write in `.flux`, rather than a JSON shape only a test
/// can construct.
fn refusal_from_program(source: &str) -> String {
    let program = match Module::parse_str(source).expect("the program parses") {
        Module::Program(p) => p,
        Module::Flow(_) => unreachable!("a program"),
    };
    match build_channels(&program.channels) {
        Ok(_) => panic!("expected a load error, but the channel was constructed"),
        Err(e) => e.to_string(),
    }
}

/// Assert the error is about the defect under test — and specifically is **not** a bind failure,
/// which is what a constructor that bound before validating would report instead.
fn assert_refused_for(text: &str, needle: &str) {
    assert!(
        text.contains(needle),
        "the refusal must name the defect ({needle:?}): {text}"
    );
    assert!(
        !text.contains("in use") && !text.contains(": bind "),
        "the refusal must precede any bind: {text}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────────────────────────
// The named failing-first test
// ─────────────────────────────────────────────────────────────────────────────────────────────────

/// D-216's failing-first test.
///
/// A `webhook` binding that states no verification is refused at **load**. The producing repository
/// already refuses it — `VerificationScheme` is a tri-state and an unset one on a webhook is a
/// loader error there — and the whole point of this arm is that flux cannot be the bypass: the
/// refusal is reproduced against the bytes on disk.
///
/// The assertion is on the constructor's result and on the port, never on a request: an arm that
/// bound first and validated on the first delivery would already have exposed an unauthenticated
/// endpoint by the time it noticed. See the module docs for why the port assertion does not race.
#[test]
fn unverified_webhook_binding_is_refused_at_load() {
    let held = std::net::TcpListener::bind("127.0.0.1:0").expect("hold a port for the whole test");
    let addr = held.local_addr().expect("the held address");

    let text = refusal(decl(json!({
        "manifest": fixture("acme-unverified.connector.toml"),
        "addr": addr.to_string(),
    })));

    assert_refused_for(&text, "states no verification");
    assert!(
        text.contains("silence is never a verification answer"),
        "the refusal says why: {text}"
    );
    drop(held);
}

// ─────────────────────────────────────────────────────────────────────────────────────────────────
// The manifest is untrusted input for path purposes
// ─────────────────────────────────────────────────────────────────────────────────────────────────

/// The `connector` setting is validated against a name grammar **before** it is joined onto the
/// connectors directory. `connector = "../../etc"` must be refused, not resolved.
///
/// The assertion is deliberately on the *grammar*, not merely on `is_err()`: an implementation that
/// happily built `~/.flux/connectors/../../etc.connector.toml` and then reported "no manifest" would
/// also return `Err`, while having done exactly the path join this test exists to forbid.
#[test]
fn connector_name_cannot_traverse() {
    for bad in ["../../etc", "..", "a/b", "/etc/passwd", "~", "acme/../../x"] {
        let text = refusal(decl(json!({ "connector": bad })));
        assert!(
            text.contains("filename"),
            "{bad:?} must be refused by the name grammar, not resolved into a path: {text}"
        );
        assert!(
            !text.contains("no manifest"),
            "{bad:?} must never reach a filesystem lookup: {text}"
        );
    }
}

/// The service name is joined onto the same directory, so it takes the same grammar.
#[test]
fn service_name_cannot_traverse() {
    let text = refusal(decl(json!({ "service": "../../etc" })));
    assert!(text.contains("filename"), "{text}");
}

// ─────────────────────────────────────────────────────────────────────────────────────────────────
// The refusal table
// ─────────────────────────────────────────────────────────────────────────────────────────────────

/// `connector` and `binding` are the two settings with no default: without either there is no
/// manifest to open and no binding to load, so both are refused before any path is built.
#[test]
fn connector_and_binding_are_required_settings() {
    for (missing, settings) in [
        (
            "connector",
            json!({ "binding": "events-api", "addr": "127.0.0.1:0" }),
        ),
        (
            "binding",
            json!({ "connector": "acme", "addr": "127.0.0.1:0" }),
        ),
    ] {
        let text = refusal(ChannelDecl {
            name: "support".to_string(),
            kind: "connector".to_string(),
            settings,
        });
        assert!(
            text.contains(&format!("missing field `{missing}`")),
            "a `connector` channel with no `{missing}` must not load: {text}"
        );
    }
}

#[test]
fn an_uninstalled_connector_is_refused() {
    let (scratch, _) = Scratch::manifest("");
    let missing = scratch.0.join("nothing-here.connector.toml");
    let text = refusal(decl(json!({ "manifest": missing.to_str().unwrap() })));
    assert_refused_for(&text, "no manifest for connector `acme`");
}

#[test]
fn an_unknown_binding_is_refused_and_names_what_exists() {
    let text = refusal(decl(json!({
        "manifest": fixture("acme.connector.toml"),
        "binding": "webhooks",
    })));
    assert_refused_for(&text, "declares no binding `webhooks`");
    assert!(
        text.contains("events-api"),
        "the refusal names what the connector does declare: {text}"
    );
}

#[test]
fn a_poll_transport_is_refused() {
    let (_scratch, path) = Scratch::manifest(&manifest_with(
        r#"[[channels]]
name = "events-api"
transport = "poll"
cursor = "acme-messages-post"
interval = "5m"

[channels.verification]
kind = "connection"
verified = true
"#,
    ));
    let text = refusal(decl(json!({ "manifest": path })));
    assert_refused_for(&text, "cannot serve");
    assert!(
        text.contains("schedule") && text.contains("cursor"),
        "the refusal says what a poll actually needs: {text}"
    );
}

/// The closed transport set has no default arm that guesses: a transport nobody modelled is refused
/// by name rather than served as whatever happens to be closest.
#[test]
fn an_unknown_transport_is_refused() {
    let (_scratch, path) = Scratch::manifest(&manifest_with(
        r#"[[channels]]
name = "events-api"
transport = "carrier-pigeon"

[channels.verification]
kind = "none"
verified = false
"#,
    ));
    assert_refused_for(
        &refusal(decl(json!({ "manifest": path }))),
        "unknown transport `carrier-pigeon`",
    );
}

#[test]
fn a_socket_transport_is_refused() {
    let (_scratch, path) = Scratch::manifest(&manifest_with(
        r#"[[channels]]
name = "events-api"
transport = "socket"

[channels.verification]
kind = "connection"
verified = true
"#,
    ));
    assert_refused_for(&refusal(decl(json!({ "manifest": path }))), "socket");
}

/// A `connection` verification is coherent for a socket or a poll and incoherent on a webhook —
/// nothing proves who called an open endpoint.
#[test]
fn a_connection_verification_on_a_webhook_is_refused() {
    let (_scratch, path) = Scratch::manifest(&manifest_with(
        r#"[[channels]]
name = "events-api"
transport = "webhook"

[channels.verification]
kind = "connection"
verified = true
"#,
    ));
    assert_refused_for(
        &refusal(decl(json!({ "manifest": path }))),
        "belongs to a socket or a poll",
    );
}

/// `verification.kind` is a closed vocabulary. An unrecognised token is refused rather than treated
/// as "not `hmac`, therefore fine" — which is how a typo becomes an unverified public endpoint.
#[test]
fn an_unknown_verification_kind_is_refused() {
    let (_scratch, path) = Scratch::manifest(&manifest_with(
        r#"[[channels]]
name = "events-api"
transport = "webhook"

[channels.verification]
kind = "hmac256"
verified = true
"#,
    ));
    assert_refused_for(
        &refusal(decl(json!({ "manifest": path }))),
        "unknown `verification.kind = \"hmac256\"`",
    );
}

/// `kind` and `verified` are one value the emitter writes twice, so a file where they disagree is a
/// file someone edited. `kind = "none"` with `verified = true` is the dangerous direction: it tells
/// anything reading the boolean alone that this endpoint is authenticated, while declaring nothing
/// that would authenticate it.
#[test]
fn an_incoherent_verification_pair_is_refused() {
    for (kind, verified) in [("none", "true"), ("connection", "false")] {
        let (_scratch, path) = Scratch::manifest(&manifest_with(&format!(
            r#"[[channels]]
name = "events-api"
transport = "webhook"

[channels.verification]
kind = "{kind}"
verified = {verified}
"#
        )));
        let text = refusal(decl(json!({ "manifest": path })));
        assert_refused_for(&text, "cannot have been emitted");
        assert!(
            text.contains(&format!("verified = {verified}")),
            "the refusal quotes the pair it refuses: {text}"
        );
    }
}

/// …and a coherent pair is not disturbed: the fixtures every other test in this file loads state
/// `kind = "none", verified = false`, and an absent `verified` is "not stated" rather than `false`.
#[test]
fn an_unstated_verified_flag_is_not_an_assertion() {
    let (_scratch, path) = Scratch::manifest(&manifest_with(
        r#"[[channels]]
name = "events-api"
transport = "webhook"
events = ["mention"]

[channels.verification]
kind = "none"
"#,
    ));
    build_channels(&[decl(json!({ "manifest": path }))])
        .expect("an omitted `verified` states nothing, so there is nothing to contradict");
}

/// The HMAC parameters name a credential; without this deployment's mapping the signature check
/// would fail open on the first delivery.
#[test]
fn a_credential_the_binding_names_with_no_entry_is_refused() {
    let (_scratch, path) = Scratch::manifest(&manifest_with(&hmac_binding(
        r#"signed = "v0:{timestamp}:{body}"
tolerance = "5m"

[channels.verification.hmac.timestamp]
source = "header"
name = "X-Acme-Timestamp"
"#,
    )));
    let text = refusal(decl(json!({ "manifest": path })));
    assert_refused_for(&text, "acme.signing_secret");
    assert!(text.contains("credentials"), "{text}");
}

/// A timestamped scheme with no window is a signature that replays forever — worse than not
/// timestamping at all, because it reads as though replay were handled.
#[test]
fn a_timestamped_signature_without_tolerance_is_refused() {
    let (_scratch, path) = Scratch::manifest(&manifest_with(&hmac_binding(
        r#"signed = "v0:{timestamp}:{body}"

[channels.verification.hmac.timestamp]
source = "header"
name = "X-Acme-Timestamp"
"#,
    )));
    assert_refused_for(&refusal(decl(credentialled(&path))), "no `tolerance`");
}

/// A window nobody can apply reads as convincingly as one that works.
#[test]
fn an_unparseable_tolerance_is_refused() {
    let (_scratch, path) = Scratch::manifest(&manifest_with(&hmac_binding(
        r#"signed = "v0:{timestamp}:{body}"
tolerance = "banana"

[channels.verification.hmac.timestamp]
source = "header"
name = "X-Acme-Timestamp"
"#,
    )));
    assert_refused_for(&refusal(decl(credentialled(&path))), "not a duration");
}

/// A body-sourced timestamp has to be parsed *before* the bytes carrying it are verified, which
/// inverts the order that makes verification mean anything and exposes a parser to any anonymous
/// caller.
#[test]
fn a_body_sourced_timestamp_is_refused() {
    let (_scratch, path) = Scratch::manifest(&manifest_with(&hmac_binding(
        r#"signed = "v0:{timestamp}:{body}"
tolerance = "5m"

[channels.verification.hmac.timestamp]
source = "body"
name = "event.ts"
"#,
    )));
    assert_refused_for(
        &refusal(decl(credentialled(&path))),
        "before they are authenticated",
    );
}

/// The load-bearing rule of `HmacSpec`: a template that never interpolates `{body}` signs a string
/// the payload never enters, so one captured signature verifies every forged payload. The defect
/// needs no typo, and everything else about the declaration reads as correct.
#[test]
fn a_signed_template_without_the_body_is_refused() {
    let (_scratch, path) = Scratch::manifest(&manifest_with(&hmac_binding(
        r#"signed = "v0:{timestamp}"
tolerance = "5m"

[channels.verification.hmac.timestamp]
source = "header"
name = "X-Acme-Timestamp"
"#,
    )));
    assert_refused_for(&refusal(decl(credentialled(&path))), "every forged payload");
}

#[test]
fn an_unknown_template_placeholder_is_refused() {
    let (_scratch, path) = Scratch::manifest(&manifest_with(&hmac_binding(
        r#"signed = "{nonce}:{body}"
"#,
    )));
    assert_refused_for(&refusal(decl(credentialled(&path))), "{nonce}");
}

/// Every structural rule about the spec passes; flux still has no verifier to feed it to, so the
/// endpoint is refused rather than bound unverified. This is what keeps the arm honest while
/// C-291/C-292 are open — see the adapter's module docs.
#[test]
fn a_well_formed_hmac_binding_is_refused_until_a_verifier_exists() {
    let (_scratch, path) = Scratch::manifest(&manifest_with(&hmac_binding(
        r#"signed = "v0:{timestamp}:{body}"
tolerance = "5m"
timestamp_format = "unix_seconds"

[channels.verification.hmac.timestamp]
source = "header"
name = "X-Acme-Timestamp"
"#,
    )));
    let text = refusal(decl(credentialled(&path)));
    assert_refused_for(&text, "C-291");
    assert!(
        text.contains("unsigned deliveries"),
        "the refusal says what binding anyway would mean: {text}"
    );
}

/// A reply that binds a symbol the payload map never declares is a dangling reply — it would load
/// and then fail on the first delivery.
#[test]
fn a_reply_binding_an_undeclared_symbol_is_refused() {
    let (_scratch, path) = Scratch::manifest(&manifest_with(
        r#"[[channels]]
name = "events-api"
transport = "webhook"
events = ["mention"]

[channels.verification]
kind = "none"
verified = false

[channels.payload]
text = "event.text"

[channels.reply]
operation = "acme-messages-post"

[channels.reply.bind]
channel = "channel"
"#,
    ));
    assert_refused_for(
        &refusal(decl(json!({ "manifest": path }))),
        "which its `[channels.payload]` map does not declare",
    );
}

/// A reply naming an operation the connector does not publish is the same defect one level up.
#[test]
fn a_reply_naming_an_unpublished_operation_is_refused() {
    let (_scratch, path) = Scratch::manifest(&manifest_with(
        r#"[[channels]]
name = "events-api"
transport = "webhook"
events = ["mention"]

[channels.verification]
kind = "none"
verified = false

[channels.reply]
operation = "acme-command-invoke"
"#,
    ));
    assert_refused_for(
        &refusal(decl(json!({ "manifest": path }))),
        "does not publish",
    );
}

#[test]
fn a_payload_path_that_fails_the_grammar_is_refused() {
    for bad in ["event..text", ".text", "text.", "event text"] {
        let (_scratch, path) = Scratch::manifest(&manifest_with(&format!(
            r#"[[channels]]
name = "events-api"
transport = "webhook"
events = ["mention"]

[channels.verification]
kind = "none"
verified = false

[channels.payload]
text = "{bad}"
"#
        )));
        let text = refusal(decl(json!({ "manifest": path })));
        assert!(
            text.contains("empty segment") || text.contains("whitespace"),
            "{bad:?} must fail the dotted-path grammar: {text}"
        );
    }
}

/// A payload key becomes a symbol a journey reads, so it has to be spellable as one — `$a-b` reads
/// as a subtraction, and `$Text` is not a name flux resolves.
#[test]
fn a_payload_symbol_that_is_not_a_flux_symbol_is_refused() {
    for bad in ["Text", "a-b", "9lives", "with space"] {
        let (_scratch, path) = Scratch::manifest(&manifest_with(&format!(
            r#"[[channels]]
name = "events-api"
transport = "webhook"
events = ["mention"]

[channels.verification]
kind = "none"
verified = false

[channels.payload]
"{bad}" = "event.text"
"#
        )));
        assert_refused_for(
            &refusal(decl(json!({ "manifest": path }))),
            "which is not a Flux symbol",
        );
    }
}

/// The delivery id lands in the payload under a reserved symbol. A binding that also declares that
/// symbol would have one silently overwrite the other, so the collision is a load error.
#[test]
fn a_delivery_id_colliding_with_a_payload_symbol_is_refused() {
    let (_scratch, path) = Scratch::manifest(&manifest_with(
        r#"[[channels]]
name = "events-api"
transport = "webhook"
events = ["mention"]

[channels.verification]
kind = "none"
verified = false

[channels.delivery_id]
source = "body"
name = "event_id"

[channels.payload]
delivery_id = "event.id"
"#,
    ));
    assert_refused_for(
        &refusal(decl(json!({ "manifest": path }))),
        "one would silently overwrite the other",
    );
}

/// A header nothing can parse resolves to nothing on **every** delivery. For a discriminator that is
/// a channel which silently never fires; for a signature header it is a check that fails open. Both
/// read as working, so both are refused at load.
#[test]
fn a_selector_naming_an_unparseable_header_is_refused() {
    let (_scratch, path) = Scratch::manifest(&manifest_with(
        r#"[[channels]]
name = "events-api"
transport = "webhook"
events = ["mention"]

[channels.verification]
kind = "none"
verified = false

[channels.discriminator]
source = "header"
name = "X Acme Event"
"#,
    ));
    assert_refused_for(
        &refusal(decl(json!({ "manifest": path }))),
        "not a valid HTTP header name",
    );
}

/// `EventDecl::when` narrows a coarse vendor event into a distinct one (GitHub's single `issues`
/// event becoming `issues.opened`). This build cannot match a narrowing — and **ignoring** one is
/// the worse failure, because the discriminator would then carry the coarse value, which is not in
/// the closed event set, so every delivery would be a no-op indistinguishable from a vendor sending
/// something nobody subscribed to. So it is refused, loudly, once.
#[test]
fn an_event_narrowed_by_a_when_condition_is_refused() {
    let (_scratch, path) = Scratch::manifest(
        r#"connector = "acme"
operations = []

[[events]]
name = "issues.opened"

[events.when.action]
const = "opened"

[[channels]]
name = "events-api"
transport = "webhook"
events = ["issues.opened"]

[channels.verification]
kind = "none"
verified = false

[channels.discriminator]
source = "header"
name = "X-Acme-Event"
"#,
    );
    let text = refusal(decl(json!({ "manifest": path })));
    assert_refused_for(&text, "cannot match");
    assert!(
        text.contains("silent no-op"),
        "the refusal says what ignoring it would cost: {text}"
    );
}

/// A binding carrying an event the connector declares nowhere would let this host fire a label for
/// an event that does not exist.
#[test]
fn a_binding_carrying_an_undeclared_event_is_refused() {
    let (_scratch, path) = Scratch::manifest(&manifest_with(
        r#"[[channels]]
name = "events-api"
transport = "webhook"
events = ["mention", "ghost"]

[channels.verification]
kind = "none"
verified = false
"#,
    ));
    assert_refused_for(
        &refusal(decl(json!({ "manifest": path }))),
        "declares nowhere",
    );
}

/// The file you opened must be the connector you asked for.
#[test]
fn a_manifest_for_another_connector_is_refused() {
    let text = refusal(decl(json!({
        "manifest": fixture("widget-hooks.connector.toml"),
        "binding": "hooks",
    })));
    assert_refused_for(&text, "declares connector `widget`");
}

/// …and the service you asked for. `widget-hooks.connector.toml` declares `service = "hooks"`, so
/// asking for the connector's *default* service must not be answered by it.
#[test]
fn a_manifest_for_another_service_is_refused() {
    let text = refusal(ChannelDecl {
        name: "support".to_string(),
        kind: "connector".to_string(),
        settings: json!({
            "connector": "widget",
            "binding": "hooks",
            "manifest": fixture("widget-hooks.connector.toml"),
            "addr": "127.0.0.1:0",
        }),
    });
    assert_refused_for(&text, "declares service");
    assert!(text.contains("<default>"), "{text}");
}

/// The host auto-approves tools, so an open non-loopback listener is a remote-trigger surface. The
/// binding says it cannot be verified, so a bearer token is the only thing left that can attribute a
/// delivery — the same rule `WebhookChannel` already makes.
#[test]
fn a_non_loopback_bind_without_a_token_is_refused() {
    let text = refusal(decl(json!({
        "manifest": fixture("acme.connector.toml"),
        "addr": "0.0.0.0:8790",
    })));
    assert!(
        text.contains("non-loopback") && text.contains("token"),
        "{text}"
    );
}

/// **An empty `token` is refused before a port is bound — on loopback too.**
///
/// It is not an absent token, it is a *worse* one. The bearer check compares the presented token
/// (which is `""` when the request carries no `Authorization` header at all) against the expected
/// one, and two empty byte strings are equal — so `Some("")` would authenticate every anonymous
/// caller while the channel reads, everywhere it is printed, as token-protected. `token secret "K"`
/// with `K` exported empty is the same value by a longer route: `flux_app::resolve_secrets` goes
/// through `std::env::var`, which does not filter an empty result.
///
/// The refusal is asserted on **both** binds deliberately. The non-loopback case is the exposure;
/// the loopback case is the one that would otherwise ship an operator a channel they believe is
/// authenticated, and be promoted to a public bind later by an `addr` edit.
#[test]
fn an_empty_token_is_refused_before_a_port_is_bound() {
    for token in ["", " ", "\t\n"] {
        for addr in ["127.0.0.1:0", "0.0.0.0:8790"] {
            let text = refusal(decl(json!({
                "manifest": fixture("acme.connector.toml"),
                "addr": addr,
                "token": token,
            })));
            assert_refused_for(&text, "set but empty");
            assert!(
                text.contains("no `Authorization` header at all"),
                "the refusal says exactly what an empty token would admit: {text}"
            );
        }
    }
}

/// …and the token is what lifts that refusal, so the rule is a requirement rather than a ban on
/// public binds.
#[test]
fn a_non_loopback_bind_with_a_token_loads() {
    let built = build_channels(&[decl(json!({
        "manifest": fixture("acme.connector.toml"),
        "addr": "0.0.0.0:8790",
        "token": "t0ken",
    }))])
    .expect("a token attributes the delivery, so the public bind is allowed");
    assert_eq!(built.len(), 1);
}

// ─────────────────────────────────────────────────────────────────────────────────────────────────
// Credentials are the operator's, and they are secrets
// ─────────────────────────────────────────────────────────────────────────────────────────────────

/// The `credentials` record is **writable in a program**, and it is what the binding's named
/// credential is satisfied from.
///
/// Both halves matter and neither proves the other: the first shows a program-declared record
/// reaching `ConnectorSettings::credentials` (the refusal moves past the credential gate all the way
/// to the missing-verifier one); the second shows that without the record the *same* manifest is
/// refused for the credential it names. A settings struct that silently ignored the record would
/// pass one of these and fail the other.
#[test]
fn a_program_declared_credentials_record_satisfies_the_binding() {
    let (_scratch, path) = Scratch::manifest(&manifest_with(&hmac_binding(
        r#"signed = "v0:{body}"
"#,
    )));

    let credentials = "  credentials { \"acme.signing_secret\": \"s3cret\" }\n";
    let with_credentials = format!(
        r#"channel support
  kind "connector"
  connector "acme"
  binding "events-api"
  manifest {path:?}
  addr "127.0.0.1:0"
  path "/acme"
{credentials}
journey noop
  flow
    return ""
"#
    );
    let text = refusal_from_program(&with_credentials);
    assert_refused_for(&text, "C-291");

    let without = with_credentials.replace(credentials, "");
    let text = refusal_from_program(&without);
    assert_refused_for(&text, "acme.signing_secret");
    assert!(text.contains("does not map"), "{text}");
}

/// **The remediation the refusal prints must itself parse.**
///
/// The credential refusal hands the operator a `credentials { … }` line to paste. A Flux record
/// separates key from value with `:`, so an `=` there is a parse error delivered at the exact moment
/// the operator has just been told what to write — the worst possible place for a typo, and one no
/// test of the *adapter* would ever catch.
///
/// So the snippet is lifted out of the error message itself and parsed, rather than restated here.
/// A restatement would drift; this cannot.
#[test]
fn the_remediation_the_credential_refusal_prints_is_parseable_flux() {
    let (_scratch, path) = Scratch::manifest(&manifest_with(&hmac_binding(
        r#"signed = "v0:{body}"
"#,
    )));
    let text = refusal(decl(json!({ "manifest": path })));

    let start = text
        .find("credentials {")
        .unwrap_or_else(|| panic!("the refusal offers a remediation: {text}"));
    let end = start
        + text[start..]
            .find('}')
            .unwrap_or_else(|| panic!("the remediation is closed: {text}"))
        + 1;
    let snippet = &text[start..end];
    assert!(
        snippet.contains(r#"secret "KEY""#),
        "the remediation reaches for a secret, never a literal: {snippet}"
    );

    let program = format!(
        r#"channel support
  kind "connector"
  connector "acme"
  binding "events-api"
  addr "127.0.0.1:0"
  {snippet}

journey noop
  flow
    return ""
"#
    );
    let module = Module::parse_str(&program)
        .unwrap_or_else(|e| panic!("the remediation must parse: {e}\n  {snippet}"));
    let Module::Program(program) = module else {
        unreachable!("a program")
    };
    // …and it lands where the adapter reads it, as a secret marker under the credential's own name.
    assert_eq!(
        program.channels[0].settings["credentials"]["acme.signing_secret"],
        json!({ "$secret": "KEY" }),
        "the pasted remediation produces the record `ConnectorSettings::credentials` reads"
    );
}

/// Credentials carry `secret "KEY"` references, which the host resolves before any adapter
/// deserializes. A marker that survives into `build_channels` — a caller that skipped that step —
/// is refused by name, **including inside the nested `credentials` record**, where an adapter that
/// only checked top-level settings would deserialize `{"$secret":…}` as an opaque failure or, worse,
/// as a value.
#[test]
fn an_unresolved_secret_inside_credentials_is_refused() {
    let text = refusal(decl(json!({
        "manifest": fixture("acme.connector.toml"),
        "credentials": { "acme.signing_secret": { "$secret": "ACME_SIGNING_SECRET" } },
    })));
    assert!(text.contains("unresolved secret"), "{text}");
    assert!(
        text.contains("ACME_SIGNING_SECRET"),
        "the refusal names the variable, never a value: {text}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────────────────────────
// What loads
// ─────────────────────────────────────────────────────────────────────────────────────────────────

/// The happy path, and the second half of the port assertion: a channel that *does* load still binds
/// nothing until `start` runs.
#[test]
fn a_valid_binding_loads_and_binds_nothing() {
    let held = std::net::TcpListener::bind("127.0.0.1:0").expect("hold a port for the whole test");
    let addr = held.local_addr().expect("the held address");

    let built = build_channels(&[decl(json!({
        "manifest": fixture("acme.connector.toml"),
        "addr": addr.to_string(),
    }))])
    .expect("a well-formed binding loads");

    assert_eq!(built.len(), 1);
    assert_eq!(built[0].name(), "support");
    // The reply operation the manifest declares is what `serve` asserts a tool for.
    assert_eq!(built[0].required_tool(), Some("acme-messages-post"));
    drop(held);
}

/// **Adding a second connector adds zero lines to `flux-channels`.**
///
/// `widget-hooks` differs from `acme` on every axis that could tempt a vendor branch: a named
/// service (so the manifest stem differs too), a header discriminator instead of a body path, a flat
/// envelope, no reply and no delivery id. Same arm, same settings, no new code.
#[test]
fn a_second_connector_adds_no_lines_to_flux_channels() {
    let built = build_channels(&[ChannelDecl {
        name: "builds".to_string(),
        kind: "connector".to_string(),
        settings: json!({
            "connector": "widget",
            "service": "hooks",
            "binding": "hooks",
            "manifest": fixture("widget-hooks.connector.toml"),
            "addr": "127.0.0.1:0",
            "path": "/widget",
        }),
    }])
    .expect("a second, differently-shaped connector loads through the same arm");

    assert_eq!(built.len(), 1);
    assert_eq!(built[0].name(), "builds");
    // No reply: nothing for `serve` to assert a tool for.
    assert_eq!(built[0].required_tool(), None);
}

// ─────────────────────────────────────────────────────────────────────────────────────────────────
// Routing and the delivery path
// ─────────────────────────────────────────────────────────────────────────────────────────────────

#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<(String, Value)>>,
}

#[async_trait]
impl Deliverer for Recorder {
    async fn deliver(&self, label: &str, payload: Value) -> anyhow::Result<Vec<JourneyRun>> {
        self.events.lock().await.push((label.to_string(), payload));
        Ok(vec![])
    }
}

fn channel(settings: Value) -> ConnectorChannel {
    ConnectorChannel::from_decl(&decl(settings)).expect("the fixture binding loads")
}

/// Wait for the spawned delivery to land. The handler deliberately does not await it (a channel
/// adapter must not block its protocol loop), so the assertion has to.
async fn recorded(rec: &Arc<Recorder>, want: usize) -> Vec<(String, Value)> {
    for _ in 0..200 {
        {
            let events = rec.events.lock().await;
            if events.len() >= want {
                return events.clone();
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    rec.events.lock().await.clone()
}

/// A declared discriminator value fires `"<channel>.<event>"`, and the payload is the binding's
/// declared symbol map resolved against the vendor envelope.
#[tokio::test]
async fn a_declared_event_fires_a_qualified_trigger_label() {
    let rec = Arc::new(Recorder::default());
    let app = channel(json!({ "manifest": fixture("acme.connector.toml") })).router(rec.clone());

    let resp = app
        .oneshot(
            Request::post("/acme")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "event_id": "Ev123",
                        "event": {
                            "type": "mention",
                            "text": "hi there",
                            "user": "U1",
                            "channel": "C1",
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);

    let events = recorded(&rec, 1).await;
    assert_eq!(events.len(), 1, "one delivery: {events:?}");
    assert_eq!(events[0].0, "support.mention");
    assert_eq!(events[0].1["text"], "hi there");
    assert_eq!(events[0].1["user"], "U1");
    assert_eq!(events[0].1["channel"], "C1");
    assert_eq!(events[0].1["delivery_id"], "Ev123");
}

/// **The narrowing this arm adds to C-294.** `ChannelBinding::events` is a closed set, so a
/// discriminator value outside it is a logged no-op — never a label of its own, and never a fallback
/// to the bare channel name. Without that, a vendor gets to name this host's trigger labels, and
/// sanitising the characters does not stop it.
#[tokio::test]
async fn an_undeclared_discriminator_value_delivers_nothing() {
    let rec = Arc::new(Recorder::default());
    let app = channel(json!({ "manifest": fixture("acme.connector.toml") })).router(rec.clone());

    for value in ["startup", "user_input", "not_an_event"] {
        let resp = app
            .clone()
            .oneshot(
                Request::post("/acme")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({ "event": { "type": value } }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        // A no-op, not a 500: vendors send event types nobody subscribed to, and an error teaches
        // them to retry forever.
        assert_eq!(resp.status(), StatusCode::NO_CONTENT, "for {value:?}");
    }

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        rec.events.lock().await.is_empty(),
        "an undeclared discriminator value must never reach the bus"
    );
}

/// The second connector's header discriminator and flat envelope work through the same handler.
#[tokio::test]
async fn a_header_discriminator_routes_the_second_connector() {
    let rec = Arc::new(Recorder::default());
    let built = ConnectorChannel::from_decl(&ChannelDecl {
        name: "builds".to_string(),
        kind: "connector".to_string(),
        settings: json!({
            "connector": "widget",
            "service": "hooks",
            "binding": "hooks",
            "manifest": fixture("widget-hooks.connector.toml"),
            "addr": "127.0.0.1:0",
            "path": "/widget",
        }),
    })
    .expect("the second connector loads");

    let resp = built
        .router(rec.clone())
        .oneshot(
            Request::post("/widget")
                .header("content-type", "application/json")
                .header("X-Widget-Event", "build.failed")
                .body(Body::from(
                    json!({ "project": "flux", "status": "red" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);

    let events = recorded(&rec, 1).await;
    assert_eq!(events[0].0, "builds.build.failed");
    assert_eq!(events[0].1["project"], "flux");
    assert_eq!(events[0].1["status"], "red");
}

/// A `token` gates **every** delivery, not just the first: a request with no bearer, or the wrong
/// one, is a 401 that reaches the bus with nothing. The assertion is on the delivery count rather
/// than only the status, because a handler that answered 401 *after* spawning the delivery would
/// look identical from the outside.
#[tokio::test]
async fn a_bearer_token_gates_every_delivery() {
    let rec = Arc::new(Recorder::default());
    let app = channel(json!({
        "manifest": fixture("acme.connector.toml"),
        "token": "t0ken",
    }))
    .router(rec.clone());

    let post = |bearer: Option<&str>| {
        let mut req = Request::post("/acme").header("content-type", "application/json");
        if let Some(bearer) = bearer {
            req = req.header("authorization", format!("Bearer {bearer}"));
        }
        req.body(Body::from(
            json!({ "event": { "type": "mention", "text": "hi" } }).to_string(),
        ))
        .unwrap()
    };

    for bad in [None, Some(""), Some("wrong"), Some("t0ke")] {
        let resp = app.clone().oneshot(post(bad)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "for {bad:?}");
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        rec.events.lock().await.is_empty(),
        "an unauthenticated delivery must never reach the bus"
    );

    let resp = app.oneshot(post(Some("t0ken"))).await.unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    let events = recorded(&rec, 1).await;
    assert_eq!(
        events.len(),
        1,
        "the authenticated delivery lands: {events:?}"
    );
    assert_eq!(events[0].0, "support.mention");
}

/// Deliveries are concurrent and the handler never serializes them: it spawns each `deliver` call
/// rather than awaiting it, so a slow journey cannot stall the protocol loop.
#[tokio::test]
async fn the_handler_does_not_block_on_a_slow_delivery() {
    struct Slow;
    #[async_trait]
    impl Deliverer for Slow {
        async fn deliver(&self, _label: &str, _payload: Value) -> anyhow::Result<Vec<JourneyRun>> {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            Ok(vec![])
        }
    }

    let app = channel(json!({ "manifest": fixture("acme.connector.toml") })).router(Arc::new(Slow));
    let started = std::time::Instant::now();
    let resp = app
        .oneshot(
            Request::post("/acme")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "event": { "type": "mention" } }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "the handler awaited a 30s delivery instead of spawning it: {:?}",
        started.elapsed()
    );
}

/// …and they are concurrent with **each other**, not merely with the response.
///
/// The distinction is the whole acceptance criterion and one test cannot cover both: an adapter that
/// answered immediately and then drained one queue of its own would pass the test above and fail this
/// one. Here every delivery is held open at once, and the assertion is that all of them arrive before
/// any of them is released — which only holds if the adapter adds no serialization. Bounding is the
/// App's admission limit's job (`flux_app::DeliveryLoad`), deliberately not a second queue here.
#[tokio::test]
async fn deliveries_run_concurrently_with_each_other() {
    const N: usize = 8;

    struct Gated {
        started: Mutex<usize>,
        release: tokio::sync::Semaphore,
    }
    #[async_trait]
    impl Deliverer for Gated {
        async fn deliver(&self, _label: &str, _payload: Value) -> anyhow::Result<Vec<JourneyRun>> {
            *self.started.lock().await += 1;
            // Never granted for the life of the test: every delivery stays in flight.
            let _ = self.release.acquire().await;
            Ok(vec![])
        }
    }

    let gate = Arc::new(Gated {
        started: Mutex::new(0),
        release: tokio::sync::Semaphore::new(0),
    });
    let app = channel(json!({ "manifest": fixture("acme.connector.toml") })).router(gate.clone());

    for i in 0..N {
        // Bounded: an adapter that *awaited* `deliver` would never answer, and a hanging test is a
        // worse failure report than a failing one.
        let resp = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            app.clone().oneshot(
                Request::post("/acme")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({ "event": { "type": "mention" } }).to_string(),
                    ))
                    .unwrap(),
            ),
        )
        .await
        .unwrap_or_else(|_| panic!("delivery {i} was awaited by the handler, not spawned"))
        .unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
    }

    for _ in 0..200 {
        if *gate.started.lock().await == N {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!(
        "only {} of {N} deliveries were in flight at once — the adapter serialized them",
        *gate.started.lock().await
    );
}

// ─────────────────────────────────────────────────────────────────────────────────────────────────
// The one refusal that needs the live App
// ─────────────────────────────────────────────────────────────────────────────────────────────────

/// Whether a *tool* exists is a question about the registry, which the decl-only builder does not
/// have — so it is asserted in `serve`, before any channel task is spawned. Following `a2a`'s split
/// rather than smuggling an `Arc<App>` into `build_channels`.
///
/// `acme-messages-post` is the reply operation `acme.connector.toml` declares, and no host in this
/// suite registers a tool by that name (the connector tool pack that would is not in flux).
#[tokio::test]
async fn missing_reply_tool_refuses_at_startup() {
    let held = std::net::TcpListener::bind("127.0.0.1:0").expect("hold a port for the whole test");
    let addr = held.local_addr().expect("the held address");

    let program = match Module::parse_str("journey noop\n  flow\n    return \"\"\n").unwrap() {
        Module::Program(p) => p,
        Module::Flow(_) => unreachable!("a program"),
    };
    let app = Arc::new(App::with_options(program, None, "mock", true));

    let channels = build_channels(&[decl(json!({
        "manifest": fixture("acme.connector.toml"),
        "addr": addr.to_string(),
    }))])
    .expect("the binding itself is well-formed — the missing tool is a host question");

    let err = serve(app, channels, false, CancellationToken::new())
        .await
        .expect_err("serve must refuse a channel whose reply operation has no tool");
    let text = err.to_string();
    assert!(text.contains("acme-messages-post"), "{text}");
    assert!(
        text.contains("support"),
        "the refusal names the channel: {text}"
    );

    // The refusal happened before any channel task was spawned, so the port is still ours.
    drop(held);
}
