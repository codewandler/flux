//! The credential boundary (C-312): prove a vendor credential never enters flux.
//!
//! # The asymmetry this defends
//!
//! On the connectors seam a **deployment** holds the vendor credential, injects it, and calls the
//! vendor itself. flux sends `{op, args}` to that deployment and holds exactly **one** secret on
//! the path — the deployment's own session bearer. That asymmetry *is* the safety argument for the
//! seam, and an invariant nobody tests is an invariant that decays at the next refactor.
//!
//! An operation opts in by declaring [`PlatformSourcing`] in its manifest. Two things follow:
//!
//! 1. It may not declare `secret_purposes` — refused at load by
//!    [`validate_manifest_operations`](flux_plugin_protocol::validate_manifest_operations), because
//!    a non-empty `secret_purposes` is what would hand the op `AccessKind::Secret` and a
//!    `secret.read` authority requirement.
//! 2. Its responses are **refused**, not merely redacted, when they carry credential-shaped
//!    material. Redaction hides a leak from the model; refusal says the boundary was crossed. On
//!    this seam there is no honest reading of "the deployment sent us a credential" — either it is
//!    the vendor's (which flux must never hold) or it is the session bearer coming back (which flux
//!    already has, and which has no business in a vendor response).
//!
//! # Responses are out-of-jail input, and are checked at ingest
//!
//! A response is authored by whatever the deployment talked to: injection-shaped, secret-bearing,
//! and outside flux's workspace jail. It is checked **where it enters** — the moment
//! `call_with_host` hands back a value, before the op's own `redact_fields` masking, before
//! stringification, before the executor's result redaction, before it can reach a session log, an
//! evidence observation or a tool result. That ordering is load-bearing twice over:
//!
//! - masking first would let a hostile manifest declare `redact_fields: ["token"]` and mask its
//!   way past its own boundary;
//! - checking at display instead of ingest is the C-215 mistake this story is told not to repeat —
//!   a later consumer that renders differently reintroduces what an earlier one hid.
//!
//! # What counts as credential-shaped, and why that set
//!
//! Exactly what [`Redactor`](flux_secret::Redactor) recognises, via
//! [`credential_shape`](flux_secret::Redactor::credential_shape) — which is `redact`'s own verdict
//! rather than a second recogniser (one pipeline, two views). That set is:
//!
//! - **registered values** — on this seam, the deployment session bearer;
//! - **PEM private-key blocks**;
//! - **passwords in a `scheme://user:password@host` URL authority**;
//! - **token shapes** — a known vendor prefix (`xoxb-`, `glpat-`, `ghp_`, `sk-ant-`, `eyJ…`, …) at
//!   or above that prefix's length floor, and an opaque value in an assignment whose *name*
//!   declares it a secret.
//!
//! **Why that set and not a different one:** it is the set flux already commits to hiding
//! everywhere else. A boundary that refused a *different* set would mean a shape that gets past the
//! boundary and is then only caught at display (or a shape refused here that the redactor considers
//! ordinary text, i.e. an unexplainable refusal). Tying the two to one implementation makes the two
//! decisions incapable of drifting; the cost — accepted deliberately — is that widening the
//! redactor widens this refusal too, so the redactor's own false-positive discipline (per-prefix
//! length floors, the two-halved contextual rule) is what keeps a Zendesk ticket body from being
//! refused for containing the word `password`.
//!
//! One thing the flat-text redactor cannot do, and this module adds: a response is **JSON**, so the
//! name/value pairs the contextual rule needs are already structural. `redact_patterns` recovers
//! them by tokenizing around `=`, which a JSON `"api_token": "…"` never presents. So the same two
//! public predicates — [`names_a_secret`](flux_secret::names_a_secret) and
//! [`is_opaque_material`](flux_secret::is_opaque_material) — are applied to the object entry
//! directly. Same vocabulary, applied where the structure is known instead of guessed; not a second
//! list to drift from the first.
//!
//! # Residual gaps, stated rather than implied
//!
//! - An **all-digit** or otherwise shapeless vendor credential is invisible to every heuristic here
//!   (`flux-secret` says so at `an_all_digit_credential_is_registration_only_…`). Registration is
//!   its only recourse, and flux by construction cannot register a credential it never sees. This
//!   boundary is a floor, not a proof of impossibility.
//! - A deployment that **encodes** a credential (base64, split across fields, embedded in an image)
//!   defeats shape matching. The seam's answer to a hostile deployment is not this check — it is
//!   that the operator started the deployment and holds its credentials; see
//!   `../flux-connectors/docs/designs/connectors-app.md` on the carve-out.
//! # Where the check runs — every call site, named (C-403)
//!
//! Four places in the tree call `PluginHost::call_with_host`. The scope statement lists all four,
//! because C-312's original wording ("the projected-tool path and `flux plugin call`") described
//! less than the tree contained, and its one carve-out did not cover the site it left out.
//!
//! | Call site | Boundary |
//! |---|---|
//! | `host/loading.rs` — the projected-tool path | **runs** |
//! | `flux-cli`'s `plugin_cmd.rs` — `flux plugin call` | **runs** (fresh redactor; see there) |
//! | `flux-capabilities`' `broker.rs` — `endpoint.discover` fan-out | **runs** (C-403) |
//! | `flux-capabilities`' `broker.rs` — `secret.read` | **exempt, by purpose** |
//!
//! **`secret.read` is exempt because its purpose is the thing this boundary refuses.** It is how a
//! discovered endpoint's `credential_ref` becomes a usable value, so a credential-shaped response
//! is its success case: checking it would fire on success and pass on failure. What bounds it is
//! the value's *disposition*, not its shape — deny-by-default operator grants plus first-use
//! approval on `EndpointBroker::resolve_credential_for`, audit by location only, and a result that
//! reaches host code and the redactor but never a tool result, a transcript, or the endpoint
//! registry. The reasoning is repeated at that call site, which is where a reader is tempted to
//! "fix" the asymmetry.
//!
//! **The `internal: true` carve-out, re-checked against the ops that exist.** C-312 excused a
//! host-dispatched `internal: true` op on the grounds that it is never advertised to the model and
//! its result goes to host code. That carve-out is currently **load-bearing for nothing**: the only
//! `internal: true` op anywhere in the tree is `plugin.validate`, which `host-kit`'s builder
//! auto-injects into every plugin manifest and which answers `{operation, valid, problems}`. No
//! shipped plugin declares one of its own. `internal_op` is public, so a plugin *could* — the
//! credential-returning `aws-bedrock.auth` shape `host-kit`'s docs describe is a design sketch, not
//! a plugin in this repository. Stated as a fact about today's tree rather than as a standing
//! excuse: if a plugin ever ships a platform-sourced `internal: true` op, this carve-out is the
//! first thing to re-derive rather than to inherit.

use super::*;
use flux_secret::{is_opaque_material, names_a_secret, Redactor};

/// The response field an [`PlatformSourcing::Activation`] operation must answer with.
///
/// A fixed name rather than a sniffed "whichever field looks like a URL": sniffing makes the
/// negative unprovable — a response with three URL-ish fields would have no defined subject, and a
/// hostile deployment would pick which one gets checked.
pub const ACTIVATION_URL_FIELD: &str = "authorize_url";

/// Query/fragment parameter names an authorize URL may not carry, beyond
/// [`names_a_secret`]'s vocabulary.
///
/// Just `code`. Everything else that matters — `access_token`, `id_token`, `refresh_token`,
/// `client_secret`, `password`, `api_key` — already contains a `names_a_secret` marker. `code` does
/// not, and cannot be added to that vocabulary globally (`code=` is ubiquitous in ordinary text and
/// would turn the redactor into a censoring machine). It is denied *here* because an authorize URL
/// is the request half of an OAuth exchange: an authorization code belongs to the callback, so a
/// `code` on the way *out* means the deployment is handing flux the grant instead of the prompt.
const ACTIVATION_DENIED_PARAMS: &[&str] = &["code"];

/// Refuse a platform-sourced operation's **successful** response, or accept it.
///
/// `Some(message)` is the sentence the operator and the model see; it names the shape and the JSON
/// path and **never quotes the offending value** — a refusal that echoes the credential would be
/// the leak it exists to prevent.
///
/// `None` for [`PlatformSourcing::None`]: an ordinary plugin op is unchanged by this story and
/// keeps its existing posture (redacted at dispatch, not refused).
pub fn refuse_response(
    platform: PlatformSourcing,
    plugin: &str,
    operation: &str,
    response: &Value,
    redactor: &Redactor,
) -> Option<String> {
    if platform.is_none() {
        return None;
    }
    if let Some(found) = credential_material(response, redactor, &mut Vec::new()) {
        return Some(format!(
            "plugin `{plugin}` operation `{operation}` is platform-sourced, and its response \
             carries {} at {}. On this seam the deployment resolves and injects the vendor \
             credential — flux holds only the deployment session bearer — so a credential-bearing \
             response means the boundary was crossed. The response was discarded, not redacted.",
            found.shape, found.path
        ));
    }
    if platform == PlatformSourcing::Activation {
        if let Some(why) = activation_refusal(response) {
            return Some(format!(
                "plugin `{plugin}` operation `{operation}` is the platform's activation path, \
                 which must answer with a URL for a human to open and never with token material: \
                 {}. The response was discarded.",
                // `why` quotes attacker-controlled fragments (a URL scheme, a parameter name), so
                // it goes through the redactor like every other path built from untrusted text.
                redactor.redact(&why)
            ));
        }
    }
    None
}

/// Refuse a platform-sourced operation's **error** message, or accept it.
///
/// The `err` frame is an ingest surface exactly like the `result` frame — a plugin that returns
/// `{"ok": false, "error": "<vendor 401 body>"}` would otherwise put whatever that body contains
/// straight into a tool result. Returns the message to use: the original when it is clean, a
/// value-free replacement when it is not.
pub fn scrub_error(
    platform: PlatformSourcing,
    plugin: &str,
    operation: &str,
    message: String,
    redactor: &Redactor,
) -> String {
    if platform.is_none() {
        return message;
    }
    match redactor.credential_shape(&message) {
        Some(shape) => format!(
            "plugin `{plugin}` operation `{operation}` failed, and its error message carried {shape}. \
             The message was discarded rather than redacted — a platform-sourced operation must \
             never hand flux credential material, on the success path or the failure path."
        ),
        None => message,
    }
}

/// A credential finding: what shape, and where in the response — never the value.
struct Found {
    shape: String,
    path: String,
}

/// Walk `value` and report the first credential material in it.
///
/// Two rules, applied to every node:
///
/// 1. every scalar, rendered as text, through [`Redactor::credential_shape`];
/// 2. every object entry whose **key** names a secret and whose **string value** is opaque
///    material — the structural form of `flux-secret`'s contextual rule, which its flat-text
///    tokenizer can only reach through an `=`.
///
/// Object *keys* go through rule 1 as well: a map keyed by token is a real exfiltration shape, and
/// a key is text arriving from the same untrusted place its value does.
fn credential_material(
    value: &Value,
    redactor: &Redactor,
    path: &mut Vec<String>,
) -> Option<Found> {
    // The JSON path, for the refusal message — **itself redacted**.
    //
    // A path is built from property NAMES, and a name is attacker-controlled text from the same
    // untrusted response as its value: a response keyed *by* the credential
    // (`{"xoxb-…": "…"}`) would otherwise put it into the refusal sentence, which is the leak the
    // refusal exists to prevent. Redaction rather than omission, so the shape of the path a
    // reader needs to locate the problem survives.
    //
    // **Each component is redacted separately, then joined.** Redacting the joined path would not
    // work: `.` is not a token boundary for the redactor (deliberately — it is what keeps
    // hostnames and version strings out of its mouth), so `a.xoxb-…` is one token that does not
    // *start* with a credential prefix and renders in the clear. Found by the test below.
    let here = |path: &[String]| {
        if path.is_empty() {
            "the response root".to_string()
        } else {
            let parts: Vec<String> = path.iter().map(|part| redactor.redact(part)).collect();
            format!("`{}`", parts.join("."))
        }
    };
    match value {
        Value::String(text) => redactor.credential_shape(text).map(|shape| Found {
            shape: shape.to_string(),
            path: here(path),
        }),
        // A registered value echoed back as a bare number or a bool still has to be caught; shape
        // heuristics cannot fire on these, but the registered-value pass can.
        Value::Number(_) | Value::Bool(_) => {
            redactor
                .credential_shape(&value.to_string())
                .map(|shape| Found {
                    shape: shape.to_string(),
                    path: here(path),
                })
        }
        Value::Null => None,
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                path.push(format!("[{index}]"));
                let found = credential_material(item, redactor, path);
                path.pop();
                if found.is_some() {
                    return found;
                }
            }
            None
        }
        Value::Object(map) => {
            for (key, val) in map {
                path.push(key.clone());
                // Rule 1 on the key itself.
                if let Some(shape) = redactor.credential_shape(key) {
                    let found = Found {
                        shape: format!("{shape} in a property NAME"),
                        path: here(path),
                    };
                    path.pop();
                    return Some(found);
                }
                // Rule 2: the contextual rule, applied to structure rather than to an `=`.
                if let Some(text) = val.as_str() {
                    if names_a_secret(key) && is_opaque_material(text) {
                        let found = Found {
                            // Rule 1 ran first and did not fire, so `key` is provably not
                            // credential material — redacted anyway, because that proof is an
                            // ordering fact a later edit could silently invalidate.
                            shape: redactor.redact(&format!(
                                "an opaque value under a property named `{key}`, which declares it \
                                 credential material"
                            )),
                            path: here(path),
                        };
                        path.pop();
                        return Some(found);
                    }
                }
                let found = credential_material(val, redactor, path);
                path.pop();
                if found.is_some() {
                    return found;
                }
            }
            None
        }
    }
}

/// Why an activation response is not "a URL for a human", or `None` if it is.
///
/// Runs *after* [`credential_material`], so the general rule has already refused anything
/// credential-shaped anywhere in the response. What is left for this function is the structural
/// half the general rule cannot see: an authorize URL that is well-formed and carries no
/// *opaque-looking* value, but is still a grant rather than a prompt.
fn activation_refusal(response: &Value) -> Option<String> {
    let Some(raw) = response.get(ACTIVATION_URL_FIELD).and_then(Value::as_str) else {
        return Some(format!(
            "the response carries no `{ACTIVATION_URL_FIELD}` string"
        ));
    };
    let Ok(url) = url::Url::parse(raw) else {
        return Some(format!("`{ACTIVATION_URL_FIELD}` is not a parseable URL"));
    };
    if !matches!(url.scheme(), "http" | "https") {
        return Some(format!(
            "`{ACTIVATION_URL_FIELD}` uses the `{}` scheme; a human opens `http`/`https`",
            url.scheme()
        ));
    }
    // Userinfo in the authority is credential placement by the URL grammar, whether or not the
    // password half looks opaque enough for the redactor's heuristics.
    if !url.username().is_empty() || url.password().is_some() {
        return Some(format!(
            "`{ACTIVATION_URL_FIELD}` carries credentials in its authority (`user:password@host`)"
        ));
    }
    for name in url.query_pairs().map(|(name, _)| name.into_owned()) {
        if denied_activation_param(&name) {
            return Some(format!(
                "`{ACTIVATION_URL_FIELD}` carries a `{name}` query parameter, which is token \
                 material, not a prompt"
            ));
        }
    }
    // The implicit-flow shape: `#access_token=…`. `Url` does not parse the fragment, so read it as
    // a form body — the same encoding an implicit-flow redirect uses.
    if let Some(fragment) = url.fragment() {
        for name in
            url::form_urlencoded::parse(fragment.as_bytes()).map(|(name, _)| name.into_owned())
        {
            if denied_activation_param(&name) {
                return Some(format!(
                    "`{ACTIVATION_URL_FIELD}` carries a `{name}` fragment parameter, which is token \
                     material, not a prompt"
                ));
            }
        }
    }
    None
}

/// Whether an authorize URL may carry a parameter of this name.
fn denied_activation_param(name: &str) -> bool {
    names_a_secret(name)
        || ACTIVATION_DENIED_PARAMS
            .iter()
            .any(|denied| name.eq_ignore_ascii_case(denied))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A vendor token flux must never hold. Joined at compile time (C-325) so a forge's secret
    /// scanning finds no credential in this file while the boundary receives identical bytes.
    fn vendor_token() -> String {
        concat!("xoxb", "-3141592653-2718281828-abcdefghijklmnopqrstuvwx").to_string()
    }

    fn refuse(platform: PlatformSourcing, response: &Value) -> Option<String> {
        refuse_response(
            platform,
            "connectors",
            "zendesk.ticket.create",
            response,
            &Redactor::new(),
        )
    }

    #[test]
    fn an_ordinary_op_is_not_subject_to_the_boundary() {
        let leaky = json!({ "token": vendor_token() });
        assert!(
            refuse(PlatformSourcing::None, &leaky).is_none(),
            "the boundary must apply only where a manifest declares it"
        );
    }

    #[test]
    fn a_credential_shape_anywhere_in_the_response_is_refused() {
        for response in [
            json!({ "token": vendor_token() }),
            json!({ "ticket": { "audit": [{ "note": format!("used {}", vendor_token()) }] } }),
            json!([{ "deep": [[{ "x": vendor_token() }]] }]),
            json!({ vendor_token(): "a map keyed by the credential" }),
            json!({ "dsn": "postgres://svc:hunter2pass@db.internal:5432/app" }),
        ] {
            let refusal = refuse(PlatformSourcing::Operation, &response)
                .unwrap_or_else(|| panic!("not refused: {response}"));
            assert!(
                !refusal.contains(&vendor_token()) && !refusal.contains("hunter2pass"),
                "the refusal quoted the credential: {refusal}"
            );
        }
    }

    /// The refusal sentence is built from the JSON **path**, and a path is made of property names
    /// that arrived in the same untrusted response as their values. A response keyed *by* the
    /// credential must not put it into the message — that would be the leak the refusal exists to
    /// prevent, dressed as a diagnostic.
    #[test]
    fn a_refusal_never_quotes_a_credential_that_arrived_as_a_property_name() {
        let deep = json!({ "a": { vendor_token(): { "b": [ { "c": 1 } ] } } });
        let refusal = refuse(PlatformSourcing::Operation, &deep).expect("refused");
        assert!(
            !refusal.contains(&vendor_token()),
            "leaked in the path: {refusal}"
        );
        assert!(
            refusal.contains("[redacted]"),
            "the path lost its shape: {refusal}"
        );
    }

    /// The structural half of the contextual rule: JSON never presents `name=value`, so a
    /// prefix-less opaque credential under a secret-naming property is only reachable this way.
    #[test]
    fn an_opaque_value_under_a_secret_naming_property_is_refused() {
        let response =
            json!({ "auth": { "api_token": "wJalrXUtnFEMI0K7MDENGbPxRfiCYEXAMPLEKEY" } });
        let refusal = refuse(PlatformSourcing::Operation, &response).expect("refused");
        assert!(refusal.contains("api_token"), "{refusal}");
        assert!(
            !refusal.contains("wJalrXUtnFEMI0K7"),
            "quoted the value: {refusal}"
        );
        // The other half of the rule still applies: a secret-naming property holding ordinary
        // config is not credential material, and refusing it would make the seam unusable.
        for benign in [
            json!({ "auth": { "api_token": "unset" } }),
            json!({ "token_path": "/etc/connectors/creds" }),
            json!({ "secret_ttl": 3600 }),
        ] {
            assert!(
                refuse(PlatformSourcing::Operation, &benign).is_none(),
                "over-refused: {benign}"
            );
        }
    }

    /// The whole point of refusing rather than redacting is that ordinary vendor payloads still
    /// flow. A boundary that refuses everything is a boundary nobody keeps.
    #[test]
    fn an_ordinary_vendor_payload_passes() {
        let response = json!({
            "ticket": { "id": 4711, "subject": "Password reset help", "status": "open" },
            "url": "https://example.zendesk.com/api/v2/tickets/4711.json",
            "requester": { "email": "someone@example.com", "name": "Some One" },
        });
        assert_eq!(refuse(PlatformSourcing::Operation, &response), None);
    }

    #[test]
    fn an_activation_response_must_be_a_url_for_a_human() {
        let good = json!({
            "authorize_url": "https://vendor.example.com/oauth/authorize?client_id=flux&state=n0nce",
            "expires_in": 600,
        });
        assert_eq!(refuse(PlatformSourcing::Activation, &good), None);

        for (bad, why) in [
            (json!({ "status": "pending" }), "no url at all"),
            (json!({ "authorize_url": "not a url" }), "unparseable"),
            (
                json!({ "authorize_url": "javascript:alert(1)" }),
                "non-http scheme",
            ),
            (
                json!({ "authorize_url": "https://u:p4ss@vendor.example.com/oauth/authorize" }),
                "userinfo",
            ),
            (
                json!({ "authorize_url": "https://vendor.example.com/oauth/authorize?code=abc123" }),
                "an authorization code on the way out",
            ),
            (
                json!({ "authorize_url": "https://vendor.example.com/cb#access_token=short" }),
                "an implicit-flow token in the fragment, too short for the opaque-material floor",
            ),
        ] {
            assert!(
                refuse(PlatformSourcing::Activation, &bad).is_some(),
                "accepted ({why}): {bad}"
            );
        }
    }

    /// The general rule runs first, so a token *beside* the URL is refused as credential material
    /// rather than needing the activation rule to know about it.
    #[test]
    fn an_activation_response_carrying_a_token_beside_the_url_is_refused() {
        let response = json!({
            "authorize_url": "https://vendor.example.com/oauth/authorize?client_id=flux",
            "access_token": vendor_token(),
        });
        let refusal = refuse(PlatformSourcing::Activation, &response).expect("refused");
        assert!(!refusal.contains(&vendor_token()), "quoted it: {refusal}");
    }

    #[test]
    fn an_error_message_carrying_credential_material_is_replaced() {
        let redactor = Redactor::new();
        let raw = format!("401 from vendor: bad token {}", vendor_token());
        let scrubbed = scrub_error(
            PlatformSourcing::Operation,
            "connectors",
            "zendesk.ticket.create",
            raw.clone(),
            &redactor,
        );
        assert!(!scrubbed.contains(&vendor_token()), "{scrubbed}");
        // A clean message is passed through untouched — the operator needs the diagnosis.
        let clean = "404 from vendor: no such ticket".to_string();
        assert_eq!(
            scrub_error(
                PlatformSourcing::Operation,
                "connectors",
                "zendesk.ticket.create",
                clean.clone(),
                &redactor
            ),
            clean
        );
        // And an ordinary op's errors are not this story's business.
        assert_eq!(
            scrub_error(
                PlatformSourcing::None,
                "connectors",
                "zendesk.ticket.create",
                raw.clone(),
                &redactor
            ),
            raw
        );
    }

    /// The one secret flux *does* hold on this path. A deployment that echoes the session bearer
    /// back inside a vendor response is refused too — it has no business there, and the registered
    /// pass is the only thing that can see a bearer with no vendor shape of its own.
    #[test]
    fn the_deployment_session_bearer_coming_back_is_refused() {
        let redactor = Redactor::new();
        let bearer = "connectors-session-bearer-0a1b2c3d4e5f";
        redactor
            .try_add_secret(bearer)
            .expect("the fixture bearer must clear the registration floor");
        let response =
            json!({ "echo": { "headers": { "authorization": format!("Bearer {bearer}") } } });
        let refusal = refuse_response(
            PlatformSourcing::Operation,
            "connectors",
            "zendesk.ticket.create",
            &response,
            &redactor,
        )
        .expect("refused");
        assert!(!refusal.contains(bearer), "quoted it: {refusal}");
    }
}
