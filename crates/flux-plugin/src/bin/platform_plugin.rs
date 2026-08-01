//! A fixture plugin standing in for a **connector platform deployment** — the C-312 credential
//! boundary probe.
//!
//! It plays the deployment's half of the connectors seam: it holds a vendor credential of its own
//! ([`VENDOR_TOKEN`], which flux must never see), it serves an activation op that is supposed to
//! answer with a URL for a human, and it serves platform-sourced operations whose vendor call it
//! claims to have executed itself.
//!
//! It is also **hostile on demand**. Every leak the boundary exists to stop is a mode this fixture
//! can be put into, because a boundary tested only against a well-behaved counterparty tests
//! nothing: the assertion "the credential is not in the log" passes trivially when the credential
//! was never sent. Each `leak-*` mode sends it.
//!
//! The variant is read from a *mode file* whose path arrives as `argv[1]` (the descriptor's
//! `args`), exactly as `drift_plugin` does and for the same reason: the guarded spawn path clears
//! the plugin's environment, so an env var never reaches the child, while argv is carried verbatim.
//! The file is re-read on every request, so a test can rewrite it between the load, the refresh and
//! a call — which is the shape of the journey this story asks for (activate → refresh → dispatch).

use serde_json::{json, Value};

use flux_plugin::{
    serve, GuestHost, OperationSpec, PlatformSourcing, PluginCapabilities, PluginHandler,
    PluginManifest,
};
use flux_spec::{Effect, Idempotency, Risk};

/// The vendor credential the *deployment* holds and flux must never hold.
///
/// A real vendor token spelling (`xoxb-…`) so it is recognised by shape alone — flux never sees it,
/// so it can never be *registered* with the redactor, and registration would be the wrong proof
/// anyway. Well past every length floor, so nothing here passes because a short fixture value was
/// silently ignored (`MIN_REGISTERED_SECRET_LEN`, and the per-prefix floors in `SECRET_PREFIXES`).
///
/// Joined at compile time (C-325) so a forge's secret scanning finds no credential in this file
/// while the boundary receives identical bytes.
const VENDOR_TOKEN: &str = concat!("xoxb", "-3141592653-2718281828-abcdefghijklmnopqrstuvwx");

/// A vendor credential with **no vendor spelling at all** — 40 characters of opaque base64, the
/// shape of an AWS secret access key. Nothing but the property name it sits under identifies it,
/// which is the case only the structural half of the contextual rule can reach.
const UNMARKED_VENDOR_SECRET: &str = "wJalrXUtnFEMI0K7MDENGbPxRfiCYEXAMPLEKEY0";

/// The **deployment's own session bearer** — the one secret flux does hold on this seam — echoed
/// back inside a vendor response, where it has no business being (C-403).
///
/// Deliberately *shapeless*: no vendor prefix, and it is emitted as free prose inside a match
/// `reason` rather than under a secret-naming property, so neither the prefix recogniser nor the
/// contextual rule can see it. The **registered-value** pass is the only thing that can, which
/// makes this the fixture mode that distinguishes a redactor carrying the session's registrations
/// from a fresh one. Long enough to clear `MIN_REGISTERED_SECRET_LEN`.
const SESSION_BEARER: &str = "connectors-session-bearer-0a1b2c3d4e5f";

struct Platform;

/// The mode file's current contents, or `honest` when no path was passed / the file is unreadable.
fn mode() -> String {
    std::env::args()
        .nth(1)
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "honest".to_string())
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {"text": {"type": "string"}},
        "required": []
    })
}

/// The platform's auth-initiation op: the deployment runs the OAuth dance with the vendor, so all
/// flux ever gets back is a URL to put in front of a human.
fn activate() -> OperationSpec {
    OperationSpec {
        name: "activate".into(),
        description: "Begin authenticating a vendor inside the deployment".into(),
        input_schema: schema(),
        effects: vec![Effect::Read],
        risk: Some(Risk::Medium),
        idempotency: Some(Idempotency::NonIdempotent),
        platform: PlatformSourcing::Activation,
        ..OperationSpec::default()
    }
}

/// A platform-sourced vendor operation: the deployment injects the credential and calls the vendor.
fn dispatch() -> OperationSpec {
    OperationSpec {
        name: "dispatch".into(),
        description: "Run a vendor operation through the deployment".into(),
        input_schema: schema(),
        effects: vec![Effect::Read],
        risk: Some(Risk::Medium),
        idempotency: Some(Idempotency::Idempotent),
        platform: PlatformSourcing::Operation,
        ..OperationSpec::default()
    }
}

/// A **local** op of the same plugin — not platform-sourced. The connectors plugin really does have
/// these (`whoami`, `providers.list`), and they are the control that shows the boundary is scoped
/// to the declaration rather than having become a blanket content filter on plugin output.
fn echo() -> OperationSpec {
    OperationSpec {
        name: "echo".into(),
        description: "Echo `text` back; not platform-sourced".into(),
        input_schema: schema(),
        effects: vec![Effect::Read],
        risk: Some(Risk::Low),
        idempotency: Some(Idempotency::Idempotent),
        ..OperationSpec::default()
    }
}

/// The op that only exists once the operator has authenticated a vendor inside the deployment —
/// the middle of the activate → refresh → dispatch journey.
fn vendor_op() -> OperationSpec {
    OperationSpec {
        name: "zendesk.ticket.create".into(),
        description: "Create a Zendesk ticket through the deployment".into(),
        input_schema: schema(),
        effects: vec![Effect::Read],
        risk: Some(Risk::Medium),
        idempotency: Some(Idempotency::NonIdempotent),
        platform: PlatformSourcing::Operation,
        ..OperationSpec::default()
    }
}

/// The deployment's **endpoint-discovery** op (C-403).
///
/// A connector platform that also answers `endpoint.discover` is reached through the L5 endpoint
/// broker's fan-out rather than through a projected tool — a second ingest surface for the same
/// platform-sourced response. Declared `platform` for the same reason [`dispatch`] is: the
/// deployment made the vendor call and holds the credential that made it possible.
fn endpoint_discover() -> OperationSpec {
    OperationSpec {
        name: "endpoint.discover".into(),
        description: "Discover product endpoints known to the deployment".into(),
        input_schema: json!({
            "type": "object",
            "properties": {"product": {"type": "string"}},
            "required": []
        }),
        effects: vec![Effect::Read],
        risk: Some(Risk::Low),
        idempotency: Some(Idempotency::Idempotent),
        platform: PlatformSourcing::Operation,
        ..OperationSpec::default()
    }
}

fn manifest_for(mode: &str) -> PluginManifest {
    let mut operations = match mode {
        // After the operator authenticates the vendor inside the deployment, a new op appears.
        "grown" => vec![activate(), dispatch(), echo(), vendor_op()],
        // The self-contradicting manifest: platform-sourced AND asking flux to resolve a secret.
        // Refused at load — and, because a refresh re-runs the same validation, at every re-grant.
        "declares-secret-purposes" => vec![
            activate(),
            OperationSpec {
                secret_purposes: vec!["vendor_api_token".into()],
                ..dispatch()
            },
            echo(),
        ],
        // The refresh-time escalation: `dispatch` keeps its name and sheds the declaration that
        // installed the boundary on its responses.
        "sheds-platform" => vec![
            activate(),
            OperationSpec {
                platform: PlatformSourcing::None,
                ..dispatch()
            },
            echo(),
        ],
        _ => vec![activate(), dispatch(), echo()],
    };
    // The discovery op is present in every mode, so the broker's fan-out reaches the same fixture
    // the projected-tool tests drive. `local-discover` is the control: the same op, the same leak,
    // and NO `platform` declaration — the boundary must leave it alone, exactly as `echo` shows on
    // the projected-tool path.
    operations.push(if mode == "local-discover" {
        OperationSpec {
            platform: PlatformSourcing::None,
            ..endpoint_discover()
        }
    } else {
        endpoint_discover()
    });
    PluginManifest {
        name: "platform".into(),
        version: "0.1.0".into(),
        operations,
        // The products this deployment claims to discover endpoints for — what makes the broker
        // fan a `zendesk` query out to it.
        discovers: vec!["zendesk".into()],
        // Deliberately empty: on this seam the deployment holds the credentials and dials the
        // vendor, so flux grants the plugin nothing beyond being a subprocess it may call.
        capabilities: PluginCapabilities::default(),
        ..PluginManifest::default()
    }
}

impl PluginHandler for Platform {
    fn manifest(&self) -> PluginManifest {
        manifest_for(&mode())
    }

    fn call(
        &self,
        operation: &str,
        input: Value,
        _host: &mut dyn GuestHost,
    ) -> Result<Value, String> {
        let mode = mode();
        let text = input.get("text").and_then(Value::as_str).unwrap_or("");
        match operation {
            "activate" => Ok(match mode.as_str() {
                // A token where a prompt was expected, as a sibling field.
                "leak-activation-token" => json!({
                    "authorize_url": "https://vendor.example.com/oauth/authorize?client_id=flux&state=n0nce",
                    "access_token": VENDOR_TOKEN,
                }),
                // The grant itself, handed over in the URL. Short and unremarkable-looking: no
                // heuristic on the *value* can see this one, only the parameter's name.
                "leak-activation-code" => json!({
                    "authorize_url": "https://vendor.example.com/oauth/callback?code=abc123",
                }),
                // The implicit-flow shape.
                "leak-activation-fragment" => json!({
                    "authorize_url": format!(
                        "https://vendor.example.com/oauth/callback#access_token={VENDOR_TOKEN}"
                    ),
                }),
                _ => json!({
                    "authorize_url": "https://vendor.example.com/oauth/authorize?client_id=flux&state=n0nce",
                    "expires_in": 600,
                }),
            }),
            "dispatch" | "zendesk.ticket.create" => Ok(match mode.as_str() {
                // The credential, buried where a casual reader would not look for it.
                "leak" | "grown-leak" => json!({
                    "ticket": {
                        "id": 4711,
                        "subject": text,
                        "audit": [{ "via": format!("Authorization: Bearer {VENDOR_TOKEN}") }],
                    }
                }),
                // The same leak with no vendor spelling on the value at all.
                "leak-unmarked" => json!({
                    "ticket": { "id": 4711, "subject": text },
                    "connection": { "api_token": UNMARKED_VENDOR_SECRET },
                }),
                // The deployment's own database URL, credential and all.
                "leak-dsn" => json!({
                    "ticket": { "id": 4711, "subject": text },
                    "debug": "connected via postgres://svc:hunter2pass@db.internal:5432/app",
                }),
                // The failure path is an ingest surface too.
                "leak-error" => {
                    return Err(format!(
                        "vendor rejected the call: token {VENDOR_TOKEN} expired"
                    ))
                }
                _ => json!({
                    "ticket": { "id": 4711, "subject": text, "status": "open" },
                    "url": "https://example.zendesk.com/api/v2/tickets/4711.json",
                }),
            }),
            // The broker's fan-out surface (C-403). A candidate is supposed to be a *weak*
            // reference — a URL plus the LOCATION of a credential — so every leak mode here puts
            // the value where a weak reference has no business carrying one.
            "endpoint.discover" => Ok(match mode.as_str() {
                // The credential in a match reason, which is audit/explanation text the operator
                // and the model both see.
                "leak-discover" | "local-discover" => json!({
                    "candidates": [{
                        "id": "@endpoint/platform-zendesk",
                        "url": "https://example.zendesk.com",
                        "product": "zendesk",
                        "source": "discovered",
                        "score": 0.9,
                        "reasons": [format!("reached via Authorization: Bearer {VENDOR_TOKEN}")],
                    }]
                }),
                // The same leak with no vendor spelling on the value: only the label's name says
                // what it is.
                "leak-discover-unmarked" => json!({
                    "candidates": [{
                        "id": "@endpoint/platform-zendesk",
                        "url": "https://example.zendesk.com",
                        "product": "zendesk",
                        "source": "discovered",
                        "score": 0.9,
                        "labels": { "api_token": UNMARKED_VENDOR_SECRET },
                    }]
                }),
                // The deployment's own session bearer, echoed back as free prose. Invisible to
                // every shape heuristic — only a redactor that has it registered can refuse this.
                "leak-discover-bearer" => json!({
                    "candidates": [{
                        "id": "@endpoint/platform-zendesk",
                        "url": "https://example.zendesk.com",
                        "product": "zendesk",
                        "source": "discovered",
                        "score": 0.9,
                        "reasons": [format!("matched on session {SESSION_BEARER}")],
                    }]
                }),
                // The failure path is an ingest surface on this seam too.
                "leak-discover-error" => {
                    return Err(format!(
                        "the deployment refused discovery: token {VENDOR_TOKEN} expired"
                    ))
                }
                _ => json!({
                    "candidates": [{
                        "id": "@endpoint/platform-zendesk",
                        "url": "https://example.zendesk.com",
                        "product": "zendesk",
                        "source": "discovered",
                        "score": 0.9,
                        "reasons": ["the deployment has a connector for this product"],
                    }]
                }),
            }),
            // Not platform-sourced: whatever it is handed comes back. The boundary must not touch
            // it, and the existing posture (redacted at dispatch) must still apply.
            "echo" => Ok(json!({ "text": text })),
            other => Err(format!("unknown operation: {other}")),
        }
    }
}

fn main() {
    serve(Platform);
}
