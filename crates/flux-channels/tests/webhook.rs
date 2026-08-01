//! Webhook adapter: a POST becomes a delivery and returns the journeys' results; `async` → 202; a
//! non-loopback bind nothing authenticates is rejected; and, since C-291, a request is authenticated
//! before its body is decoded, while a `verify` declaration this build cannot honour never binds a
//! port at all.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use flux_app::JourneyRun;
use flux_channels::{Deliverer, WebhookChannel};
use flux_lang::program::{ChannelDecl, Module};
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tower::ServiceExt; // for `oneshot`

#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<(String, Value)>>,
}

#[async_trait]
impl Deliverer for Recorder {
    async fn deliver(&self, label: &str, payload: Value) -> anyhow::Result<Vec<JourneyRun>> {
        self.events.lock().await.push((label.to_string(), payload));
        Ok(vec![JourneyRun {
            journey: "j".to_string(),
            result: "done".to_string(),
            steps: 1,
            usage: None,
            model: "mock".to_string(),
        }])
    }
}

fn channel(settings: Value) -> WebhookChannel {
    WebhookChannel::from_decl(&ChannelDecl {
        name: "hook".to_string(),
        kind: "webhook".to_string(),
        settings,
    })
    .unwrap()
}

#[tokio::test]
async fn post_becomes_delivery_and_returns_runs() {
    let rec = Arc::new(Recorder::default());
    let app = channel(json!({ "addr": "127.0.0.1:0", "path": "/hook" })).router(rec.clone());

    let resp = app
        .oneshot(
            Request::post("/hook")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "x": 1 }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let events = rec.events.lock().await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, "hook");
    assert_eq!(events[0].1, json!({ "x": 1 }));
}

#[tokio::test]
async fn async_mode_returns_202() {
    let rec = Arc::new(Recorder::default());
    let app = channel(json!({ "addr": "127.0.0.1:0", "path": "/hook", "async": true })).router(rec);

    let resp = app
        .oneshot(
            Request::post("/hook")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::ACCEPTED);
}

/// **Failing-first (C-291): nothing about a request may be decoded before it is authenticated.**
///
/// `Json(body): Json<Value>` is an axum *extractor*, so it consumes and deserializes the body before
/// the handler body runs at all — which puts the decode strictly ahead of the bearer check that
/// follows it, and ahead of any signature check that could ever be added. Two probes show it from
/// outside, without needing a verifier to exist yet:
///
/// - malformed JSON with a **wrong** bearer answers `400`, so the parser ran for an unauthenticated
///   caller and reported on its input;
/// - a missing `content-type` with a wrong bearer answers `415`, so the extractor's negotiation ran
///   for one too.
///
/// Both are `401` once authentication precedes the decode. That ordering is the whole of this story:
/// a signature is over bytes, so anything that decodes first has authenticated something other than
/// what arrived — and a rejection an unauthenticated caller can *tell apart* is a probe for how far
/// its forgery got.
#[tokio::test]
async fn authentication_precedes_the_body_decode() {
    let rec = Arc::new(Recorder::default());
    let app =
        channel(json!({ "addr": "127.0.0.1:0", "path": "/hook", "token": "t0ken" })).router(rec);

    let malformed = app
        .clone()
        .oneshot(
            Request::post("/hook")
                .header("content-type", "application/json")
                .header("authorization", "Bearer wrong")
                .body(Body::from("{ this is not json"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        malformed.status(),
        StatusCode::UNAUTHORIZED,
        "an unauthenticated caller must not learn that its body failed to parse — that answer is \
         only reachable if the decode ran first"
    );

    let untyped = app
        .oneshot(
            Request::post("/hook")
                .header("authorization", "Bearer wrong")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        untyped.status(),
        StatusCode::UNAUTHORIZED,
        "nor that its content-type was unacceptable"
    );
}

#[test]
fn non_loopback_requires_token() {
    let err = WebhookChannel::from_decl(&ChannelDecl {
        name: "hook".to_string(),
        kind: "webhook".to_string(),
        settings: json!({ "addr": "0.0.0.0:8790", "path": "/hook" }),
    })
    .err()
    .expect("non-loopback bind without a token must be rejected");
    assert!(err.to_string().contains("token"), "got: {err}");
}

fn refusal(settings: Value) -> String {
    WebhookChannel::from_decl(&ChannelDecl {
        name: "hook".to_string(),
        kind: "webhook".to_string(),
        settings,
    })
    .err()
    .expect("an empty token must be refused")
    .to_string()
}

/// **An empty `token` is refused before a port is bound — on loopback too.**
///
/// It is not an absent token, it is a *worse* one: `token.is_none()` is what the non-loopback guard
/// tests, so `Some("")` sails through it and the public bind is permitted, while the handler then
/// compares the presented token (`""` when no `Authorization` header is sent) against the expected
/// one and finds them equal.
///
/// The refusal is asserted on **both** binds deliberately. The non-loopback case is the exposure; the
/// loopback case is the one that would otherwise ship an operator a channel they believe is
/// authenticated, one `addr` edit away from being public. Whitespace-only counts as empty — `" "` is
/// not a token anybody meant to configure.
#[test]
fn an_empty_token_is_refused_before_a_port_is_bound() {
    for token in ["", " ", "\t\n"] {
        for addr in ["127.0.0.1:0", "0.0.0.0:8790"] {
            let text = refusal(json!({ "addr": addr, "path": "/hook", "token": token }));
            assert!(
                text.contains("set but empty"),
                "the refusal must name the cause, got: {text}"
            );
            assert!(
                text.contains("no `Authorization` header at all"),
                "the refusal says exactly what an empty token would admit: {text}"
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────────────────
// C-291 — a declaration this build cannot honour never binds a port
// ─────────────────────────────────────────────────────────────────────────────────────────────────

/// A complete, well-formed GitHub-shaped `verify` record. Every negative case below is this record
/// with one thing changed, so each test's subject is the one field it names.
fn github_verify() -> Value {
    json!({
        "scheme": "hmac",
        "algorithm": "sha256",
        "encoding": "hex",
        "header": "X-Hub-Signature-256",
        "prefix": "sha256=",
        "signed": "{body}",
        "secret": "a-real-signing-secret",
    })
}

fn with_verify(verify: Value) -> Value {
    json!({ "addr": "127.0.0.1:0", "path": "/hook", "verify": verify })
}

/// **A declared verification this build cannot perform is a load error naming the channel.**
///
/// Failing-first: `WebhookSettings` is not `deny_unknown_fields`, so before C-291 a whole `verify`
/// record deserialized to *nothing* — the channel loaded, bound its port, and accepted every unsigned
/// delivery while the declaration said it was signed. That is strictly worse than declaring nothing,
/// because it reads as though signatures were being checked.
///
/// The refusal is the same doctrine the connector arm applies to a manifest that declares
/// `verification.kind = "hmac"` (D-216): a build with no verifier refuses to bind, it never binds and
/// skips the check. It is `from_decl`, so it happens inside `build_channels`, before any listener.
#[test]
fn a_declared_verification_this_build_cannot_perform_is_a_load_error() {
    let text = refusal(with_verify(github_verify()));
    assert!(text.contains("hook"), "names the channel: {text}");
    assert!(text.contains("cannot perform"), "{text}");
    assert!(
        text.contains("C-292"),
        "points at the story that fills the seam: {text}"
    );
    assert!(
        text.contains("unsigned deliveries"),
        "says what binding anyway would have accepted: {text}"
    );
}

/// The refusal really does happen before a port is bound — the same place an unknown channel kind is
/// refused, which is `build_channels`, not `start`.
#[test]
fn the_refusal_happens_in_build_channels_before_anything_binds() {
    let err = flux_channels::build_channels(&[ChannelDecl {
        name: "hook".to_string(),
        kind: "webhook".to_string(),
        settings: with_verify(github_verify()),
    }])
    .err()
    .expect("a channel that cannot honour its own declaration must not be built");
    assert!(err.to_string().contains("cannot perform"), "{err}");
}

/// Every structural rule about the record, each reporting its own defect rather than being masked by
/// the coarser "this build has no verifier" refusal that follows all of them.
#[test]
fn each_defect_in_a_verify_record_reports_itself() {
    let cases: [(&str, Value, &str); 9] = [
        ("unknown scheme", json!("hmac-sha256-v2"), "unknown scheme"),
        ("unknown algorithm", json!("md5"), "unknown algorithm"),
        ("unknown encoding", json!("base32"), "unknown encoding"),
        (
            "unreadable header",
            json!("X Hub Signature"),
            "not a valid HTTP header name",
        ),
        ("empty prefix", json!(""), "empty `prefix`"),
        (
            "template without a body",
            json!("{timestamp}"),
            "never interpolates",
        ),
        (
            "unknown placeholder",
            json!("{nonce}{body}"),
            "unknown placeholder",
        ),
        (
            "unclosed placeholder",
            json!("{body"),
            "unclosed placeholder",
        ),
        ("empty secret", json!(""), "empty `secret`"),
    ];
    let field = [
        "scheme",
        "algorithm",
        "encoding",
        "header",
        "prefix",
        "signed",
        "signed",
        "signed",
        "secret",
    ];
    for ((what, value, expected), field) in cases.into_iter().zip(field) {
        let mut verify = github_verify();
        verify[field] = value;
        let text = refusal(with_verify(verify));
        assert!(text.contains(expected), "{what}: {text}");
        assert!(text.contains("hook"), "{what} names the channel: {text}");
    }

    // A field a scheme cannot be performed without is refused by name, never defaulted — a default
    // here would be a guess about how a vendor signs.
    for field in [
        "scheme",
        "algorithm",
        "encoding",
        "header",
        "signed",
        "secret",
    ] {
        let mut verify = github_verify();
        verify.as_object_mut().expect("a record").remove(field);
        let text = refusal(with_verify(verify));
        assert!(text.contains(field), "a missing `{field}` is named: {text}");
    }
}

/// **A `{timestamp}` template with no `tolerance` is a load error** — C-292's rule, refused by this
/// story's loader. A timestamped scheme with no window is a signature that replays forever, which is
/// worse than not timestamping at all because it reads as though replay were handled.
#[test]
fn a_timestamped_template_without_a_usable_tolerance_is_a_load_error() {
    let mut verify = github_verify();
    verify["signed"] = json!("v0:{timestamp}:{body}");
    verify["timestamp"] = json!({ "source": "header", "name": "X-Slack-Request-Timestamp" });

    let text = refusal(with_verify(verify.clone()));
    assert!(text.contains("no `tolerance`"), "{text}");
    assert!(text.contains("replays forever"), "{text}");

    verify["tolerance"] = json!("a while");
    let text = refusal(with_verify(verify.clone()));
    assert!(text.contains("not a duration"), "{text}");

    // And a well-formed one gets all the way to the missing-verifier refusal, so the rule above is
    // about the tolerance and not about timestamped schemes in general.
    verify["tolerance"] = json!("5m");
    assert!(refusal(with_verify(verify)).contains("cannot perform"));
}

/// **A timestamp read from the body is unimplementable by construction, not merely unimplemented.**
///
/// Honouring it would parse the very bytes the signature authenticates, before they are
/// authenticated — inverting the ordering this whole story establishes, and handing a parser to any
/// anonymous caller. It is spellable, so it is refused by name.
#[test]
fn a_body_sourced_timestamp_is_a_load_error() {
    let mut verify = github_verify();
    verify["signed"] = json!("{timestamp}{body}");
    verify["tolerance"] = json!("5m");
    verify["timestamp"] = json!({ "source": "body", "name": "event.ts" });

    let text = refusal(with_verify(verify.clone()));
    assert!(text.contains("from the body"), "{text}");
    assert!(
        text.contains("before they are authenticated"),
        "the refusal states the inversion, which is the reason: {text}"
    );

    verify["timestamp"] = json!({ "source": "cookie", "name": "ts" });
    assert!(refusal(with_verify(verify.clone())).contains("unknown source"));

    // A window and a selector the template never interpolates describe a value nothing reads — and
    // read, to whoever wrote them, as replay protection that is in force.
    let mut unused = github_verify();
    unused["tolerance"] = json!("5m");
    assert!(refusal(with_verify(unused)).contains("never interpolates"));
}

/// **A signing secret the redactor would not register is refused at load, and never echoed.**
///
/// `Redactor::try_add_secret` silently registers nothing below a 6-character floor, so a shorter
/// secret is scrubbed from no log, no diff and no tool result. `flux_app::resolve_secrets` already
/// fails the load for a `secret "KEY"` *reference* that resolves too short (C-315); this is the same
/// rule stated where the value is used, which is the half that also catches one written as a literal
/// — a literal never passes through the redactor at all.
///
/// The refusal names neither the value nor its length: a diagnostic built from the secret is an
/// oracle, not a diagnostic.
#[test]
fn a_signing_secret_too_short_to_redact_is_refused_and_never_echoed() {
    let mut verify = github_verify();
    verify["secret"] = json!("pin12");
    let text = refusal(with_verify(verify));
    assert!(text.contains("shorter than 6"), "{text}");
    assert!(
        text.contains("redactor"),
        "states why the floor exists: {text}"
    );
    assert!(
        !text.contains("pin12"),
        "must never echo the secret: {text}"
    );
}

/// **`verify "none"` is a distinct, deliberate declaration.** Absent and explicitly-none must not
/// normalise together: on a non-loopback bind only the second is admissible, because the host
/// auto-approves tools and an endpoint whose verification nobody decided is a remote-trigger surface
/// nobody decided to expose.
///
/// ⚠ **Breaking**, deliberately: a non-loopback `channel webhook` that carries a `token` and no
/// `verify` used to load and now does not. Weighed and taken — the fix is one line of program text,
/// the error says exactly which line, and the alternative is that the decision stays unmade for every
/// public webhook flux has ever run. See the story's Progress note.
#[test]
fn a_non_loopback_bind_must_state_a_verification_decision() {
    let text = refusal(json!({ "addr": "0.0.0.0:8790", "path": "/hook", "token": "t0ken" }));
    assert!(text.contains("stated verification decision"), "{text}");
    assert!(text.contains("verify \"none\""), "offers the fix: {text}");

    // Stated: it loads. This is the assertion that keeps the rule from being "refuse public binds".
    WebhookChannel::from_decl(&ChannelDecl {
        name: "hook".to_string(),
        kind: "webhook".to_string(),
        settings: json!({
            "addr": "0.0.0.0:8790", "path": "/hook", "token": "t0ken", "verify": "none",
        }),
    })
    .expect("a stated `verify \"none\"` with a token is a complete decision");

    // A loopback bind is unaffected — silence there is a local endpoint, not an exposure.
    WebhookChannel::from_decl(&ChannelDecl {
        name: "hook".to_string(),
        kind: "webhook".to_string(),
        settings: json!({ "addr": "127.0.0.1:0", "path": "/hook" }),
    })
    .expect("loopback needs neither a token nor a stated verification");

    // A word that is not an answer is refused rather than read as `none`.
    let text = refusal(json!({ "addr": "127.0.0.1:0", "verify": "off" }));
    assert!(text.contains("not a verification answer"), "{text}");
}

/// **The public-bind rule is keyed on the property, not on `token.is_none()`** (C-321).
///
/// A `verify "none"` channel with no token authenticates nothing and is refused — the rule the crate
/// has always had, restated. A *verifying* channel with no token is admitted, and that is the point
/// of the story: a vendor that signs its payloads and cannot send a custom `Authorization` header now
/// has an authenticated route in. Asserted by the refusal reaching the missing-verifier message,
/// which is downstream of every bind rule.
#[test]
fn a_verifying_channel_needs_no_bearer_token_to_face_the_network() {
    let text = refusal(json!({
        "addr": "0.0.0.0:8790", "path": "/hook", "verify": github_verify(),
    }));
    assert!(
        text.contains("cannot perform"),
        "a signature is authentication, so no bind rule should have fired first: {text}"
    );

    let text = refusal(json!({ "addr": "0.0.0.0:8790", "path": "/hook", "verify": "none" }));
    assert!(
        text.contains("nothing to authenticate a delivery"),
        "but a stated `none` with no token still authenticates nothing: {text}"
    );
}

/// **The declaration needs no language change, and `secret` is recognised inside the record.**
///
/// Asserted against the lowered `ChannelDecl`, not against a hand-written settings bag: a settings
/// struct that models a record the grammar cannot express is a feature nobody can use, and the
/// `secret "KEY"` marker at that nesting depth is what keeps the signing key out of the program text.
#[test]
fn a_verify_record_is_writable_in_a_program() {
    let src = "\
channel gh
  kind \"webhook\"
  addr \"127.0.0.1:0\"
  path \"/gh\"
  verify { \"scheme\": \"hmac\", \"algorithm\": \"sha256\", \"encoding\": \"hex\", \
\"header\": \"X-Hub-Signature-256\", \"prefix\": \"sha256=\", \"signed\": \"{body}\", \
\"secret\": secret \"GITHUB_WEBHOOK_SECRET\" }

journey noop
  flow
    return \"\"
";
    let program = match Module::parse_str(src).expect("the program parses") {
        Module::Program(p) => p,
        Module::Flow(_) => unreachable!("a program"),
    };
    let settings = &program.channels[0].settings;
    assert_eq!(settings["verify"]["scheme"], json!("hmac"));
    assert_eq!(
        settings["verify"]["signed"],
        json!("{body}"),
        "the template is carried verbatim — `{{body}}` is a placeholder for the verifier, not for \
         the program's own string interpolation"
    );
    assert_eq!(
        settings["verify"]["secret"],
        json!({ "$secret": "GITHUB_WEBHOOK_SECRET" }),
        "`secret \"KEY\"` is recognised at this nesting depth, so the signing key never appears in \
         the program text"
    );

    // And an unresolved marker never reaches a listener: `build_channels` refuses it by name.
    let err = flux_channels::build_channels(&program.channels)
        .err()
        .expect("an unresolved secret marker must be refused");
    assert!(err.to_string().contains("unresolved secret"), "{err}");
    assert!(err.to_string().contains("GITHUB_WEBHOOK_SECRET"), "{err}");
}

/// The same value by the longer route an operator actually reaches by accident: `token secret "K"`
/// with `K` exported **set-and-empty**. Nobody types `""` on this path, which is what makes it the
/// dangerous spelling.
///
/// The premise is asserted rather than assumed: `flux_app::resolve_secrets` resolves a
/// `{"$secret":"K"}` marker through `std::env::var`, and `std::env::var` on a set-but-empty variable
/// returns `Ok("")` — **not** `Err(NotPresent)`. So the marker becomes the string `""` and the
/// settings deserialize to `Some("")`, landing on exactly the hole above with the operator believing
/// the channel is token-protected. `from_decl` is the same refusal either way, because by the time it
/// runs, a resolved secret and a literal are the same value.
#[test]
fn a_set_but_empty_secret_env_var_is_refused_too() {
    let key = "FLUX_TEST_C317_EMPTY_WEBHOOK_TOKEN";
    std::env::set_var(key, "");
    assert_eq!(
        std::env::var(key).as_deref(),
        Ok(""),
        "the premise of the whole defect: a set-but-empty env var resolves, it does not report \
         itself absent — so `secret \"{key}\"` becomes `Some(\"\")`, never `None`"
    );

    let resolved = std::env::var(key).expect("set above");
    let err = refusal(json!({ "addr": "0.0.0.0:8790", "path": "/hook", "token": resolved }));
    assert!(err.contains("set but empty"), "got: {err}");
    assert!(
        err.contains("secret"),
        "the refusal points at the `secret \"KEY\"` route that produced it: {err}"
    );

    std::env::remove_var(key);
}
