//! The reusable **server** side of the A2A protocol: JSON-RPC dispatch, the agent-card builder, and
//! the message/event shaping — all over [`serde_json::Value`] and a small [`A2aTurn`] seam, with
//! **no HTTP-framework dependency**. A surface (axum in `flux-server`, or another downstream HTTP
//! host) supplies the route + state and calls these functions, so the *protocol* has one
//! definition the way [`crate::types`] gives the *wire* one definition.
//!
//! The blocking [`dispatch`] handles `message/send` end-to-end (run a turn → completed [`Task`]).
//! `message/stream` (SSE) is not handled here — it needs the surface's streaming machinery — but the
//! frame shaping ([`status_update_value`], [`artifact_update_value`]) and the pure helpers
//! ([`extract_input`], [`extract_context_id`], [`history_length`], [`agent_card`], [`now_rfc3339`],
//! [`rpc_ok`]/[`rpc_err`]) are shared so a streaming surface re-uses them too.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::error;
use crate::types::{
    new_id, AgentCard, AgentInterface, Artifact, Capabilities, Message, Part, Skill, Task,
    TaskArtifactUpdateEvent, TaskState, TaskStatus, TaskStatusUpdateEvent, PROTOCOL_VERSION,
    TRANSPORT_JSONRPC,
};

/// One text turn of an agent: run a user message to the agent's final answer. The only seam
/// [`dispatch`] needs — a consumer implements it for its engine/agent. Errors are returned as a
/// message string so the dispatcher can surface them as a JSON-RPC error without this leaf crate
/// taking on an error-type dependency.
#[async_trait]
pub trait A2aTurn: Send + Sync {
    /// Run one user message to the agent's final answer text.
    async fn run(&self, input: &str) -> Result<String, String>;

    /// As [`run`](Self::run), but the reply may carry **extra parts** beside the answer text —
    /// e.g. `data` parts with typed UI blocks a chat surface renders natively. The default
    /// delegates to [`run`](Self::run) with no extra parts, so existing implementors are
    /// unaffected.
    async fn run_rich(&self, input: &str) -> Result<A2aReply, String> {
        Ok(A2aReply {
            text: self.run(input).await?,
            extra_parts: Vec::new(),
        })
    }

    /// As [`run_rich`](Self::run_rich), with the turn's **conversation identity** (A-48) —
    /// implement THIS to key multi-turn continuity on the client's `contextId` (e.g. one engine
    /// session per `contextId`, the stateful mode `flux-server` ships). The default ignores the
    /// context and delegates, so existing implementors keep today's per-turn independence
    /// unchanged; [`dispatch`] always goes through this method.
    async fn run_in_context(&self, ctx: &A2aTurnContext, input: &str) -> Result<A2aReply, String> {
        let _ = ctx;
        self.run_rich(input).await
    }
}

/// The conversation identity of one dispatched turn (A-48): what a stateful [`A2aTurn`]
/// implementor keys session continuity on. `context_id` is the request's `contextId`, when the
/// client sent one — a client that never repeats a `contextId` gets exactly the old per-turn
/// isolation.
#[derive(Debug, Clone, Default)]
pub struct A2aTurnContext {
    pub context_id: Option<String>,
    /// The authenticated realm (tenancy key) of the caller, set exclusively by the mount from
    /// its own request authentication — NEVER derived from message content. `None` = the mount
    /// runs unauthenticated/single-realm. A stateful implementor keys `contextId` continuity
    /// *within* this realm (D-69): the spec is explicit that `contextId` is a grouping
    /// mechanism, not a security boundary.
    pub realm: Option<String>,
}

/// A rich turn reply: the final answer text plus extra reply-message parts (see
/// [`A2aTurn::run_rich`]).
pub struct A2aReply {
    pub text: String,
    pub extra_parts: Vec<Part>,
}

// ── Agent card ────────────────────────────────────────────────────────────────

/// Build an absolute URL for `path` from a request's scheme + host, so a served discovery card's
/// `url` is correct whether the agent is reached directly or through a reverse proxy.
///
/// Pure and framework-free (D-57): callers resolve their own `Host`/`X-Forwarded-Proto` header
/// values (each HTTP framework has its own header-map type) and pass plain strings — this is the
/// one shared home for the scheme/host → URL derivation, used by `flux-server`'s axum handler and
/// any other A2A surface. `forwarded_proto` is the `X-Forwarded-Proto` value, if the request carries
/// one; absent, the scheme falls back to `"http"`. `path` is appended verbatim (callers pass a
/// leading `/`).
pub fn card_url(forwarded_proto: Option<&str>, host: &str, path: &str) -> String {
    let scheme = forwarded_proto.unwrap_or("http");
    format!("{scheme}://{host}{path}")
}

/// Build an A2A discovery [`AgentCard`]. `url` is the JSON-RPC endpoint (else the client derives
/// `<base>/a2a`); `version` is the serving agent's version; `skills` are `(id, name, description)`
/// triples surfaced as the card's skills (pass `id == name` when there is no separate identifier).
/// Set `streaming` only when the surface actually implements `message/stream` (the blocking
/// [`dispatch`] does not). The security fields default to `None`; an auth-enforcing surface
/// declares its schemes on the result via [`AgentCard::with_security_schemes`] /
/// [`AgentCard::with_security`].
pub fn agent_card(
    name: &str,
    description: &str,
    url: Option<String>,
    version: &str,
    skills: &[(String, String, String)],
    streaming: bool,
) -> AgentCard {
    // Declare the JSON-RPC interface flux actually serves (A-49): one `interfaces` entry and a
    // matching `preferredTransport`, both keyed to the endpoint `url`. A card with no `url` (a
    // never-served/degenerate card) declares no transport, which is the honest empty state.
    let (interfaces, preferred_transport) = match &url {
        Some(u) => (
            vec![AgentInterface {
                url: Some(u.clone()),
                transport: Some(TRANSPORT_JSONRPC.to_string()),
            }],
            Some(TRANSPORT_JSONRPC.to_string()),
        ),
        None => (Vec::new(), None),
    };
    AgentCard {
        protocol_version: PROTOCOL_VERSION.to_string(),
        name: name.to_string(),
        description: description.to_string(),
        url,
        version: version.to_string(),
        capabilities: Capabilities {
            streaming,
            push_notifications: false,
            ..Default::default()
        },
        default_input_modes: vec!["text/plain".to_string()],
        default_output_modes: vec!["text/plain".to_string()],
        skills: skills
            .iter()
            .map(|(id, name, description)| Skill {
                id: id.clone(),
                name: name.clone(),
                description: description.clone(),
                input_modes: vec!["text/plain".to_string()],
                output_modes: vec!["text/plain".to_string()],
            })
            .collect(),
        interfaces,
        preferred_transport,
        // Honest: flux has no `agent/getAuthenticatedExtendedCard` method yet.
        supports_authenticated_extended_card: Some(false),
        provider: None,
        documentation_url: None,
        icon_url: None,
        security_schemes: None,
        security: None,
    }
}

// ── JSON-RPC dispatch (blocking methods) ────────────────────────────────────────

/// Handle one JSON-RPC 2.0 request body for the **blocking** A2A methods, running `message/send`
/// through `runner`. Returns the JSON-RPC response value: success carries a completed [`Task`] (the
/// answer in `status.message`); an unknown method or bad params carry a JSON-RPC error.
///
/// `realm` is the caller's authenticated realm (tenancy key). It MUST come from the mount's
/// request-authentication layer — never from the request body — and rides
/// [`A2aTurnContext::realm`] into the runner, so a stateful implementor keys `contextId`
/// continuity within the caller's realm (D-69). Pass `None` when the mount runs
/// unauthenticated/single-realm.
pub async fn dispatch(runner: &dyn A2aTurn, realm: Option<&str>, body: &Value) -> Value {
    let id = body.get("id").cloned().unwrap_or(Value::Null);
    if body.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return rpc_err(id, -32600, "jsonrpc must be \"2.0\"");
    }
    match body
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "message/send" => send(runner, realm, id, body.get("params")).await,
        other if is_unsupported_a2a_method(other) => rpc_err(
            id,
            error::UNSUPPORTED_OPERATION,
            format!("unsupported operation: {other}"),
        ),
        other => rpc_err(id, -32601, format!("method not found: {other}")),
    }
}

/// Whether `method` is an A2A method flux **recognizes but does not implement** — so a dispatcher
/// should answer [`error::UNSUPPORTED_OPERATION`] (`-32004`) rather than the generic `-32601 Method
/// not found` (reserved for genuinely-unrecognized names). These all depend on an addressable,
/// retained task or an extended card that flux's synchronous-turn model has no notion of (A-53).
///
/// This is the **one** classifier every A2A dispatch site shares (the reusable [`dispatch`] here and
/// `flux-server`'s HTTP handlers) so the set cannot drift between them. `tasks/get` is deliberately
/// excluded: its spec-correct answer is task-retention-dependent (`-32001 TaskNotFound` vs a value
/// once tasks are retained) and arrives with the stateful task model, so it keeps today's fall-through.
pub fn is_unsupported_a2a_method(method: &str) -> bool {
    matches!(
        method,
        "tasks/cancel"
            | "tasks/resubscribe"
            | "tasks/pushNotificationConfig/set"
            | "tasks/pushNotificationConfig/get"
            | "tasks/pushNotificationConfig/list"
            | "tasks/pushNotificationConfig/delete"
            | "agent/getAuthenticatedExtendedCard"
    )
}

/// Compose the turn input from an inbound A2A `message`'s parts, or return the JSON-RPC error code
/// to refuse with — the single boundary that decides what flux's text turn runs on (A-51).
///
/// flux serves a **text** agent, so:
/// - `text` parts contribute their text;
/// - `data` parts are surfaced as their structured JSON payload (via [`render_data_part`]), so a
///   message that carries only a `data` part runs a *real* turn — the agent sees the data — instead
///   of the empty turn `extract_text` alone would have produced;
/// - `file` parts cannot be consumed by a text turn, so a message carrying one is refused with
///   [`error::CONTENT_TYPE_NOT_SUPPORTED`] (`-32005`) rather than silently dropped: the client
///   learns flux took no file, instead of getting a `completed` task that ignored it.
///
/// Refusal codes: an absent message / empty parts array is a malformed request (`-32602`); parts
/// present but none usable (a lone `file` part, or only unknown kinds) is an unusable content type
/// (`-32005`). This supersedes the old `extract_text` + `no_text_error_code` split at every dispatch
/// site, so "what do we run / how do we refuse" lives in exactly one place and cannot drift.
pub fn extract_input(params: &Value) -> Result<String, i32> {
    let Some(parts) = params
        .get("message")
        .and_then(|m| m.get("parts"))
        .and_then(Value::as_array)
    else {
        return Err(-32602);
    };
    if parts.is_empty() {
        return Err(-32602);
    }
    // A file part is unconsumable by a text turn: refuse the whole message rather than drop it,
    // even when it rides alongside text — a silently-ignored attachment is exactly the A-51 gap.
    if parts
        .iter()
        .any(|p| p.get("kind").and_then(Value::as_str) == Some("file"))
    {
        return Err(error::CONTENT_TYPE_NOT_SUPPORTED);
    }
    let mut pieces: Vec<String> = Vec::new();
    for p in parts {
        match p.get("kind").and_then(Value::as_str) {
            Some("text") => {
                if let Some(t) = p.get("text").and_then(Value::as_str) {
                    pieces.push(t.to_string());
                }
            }
            // Surface the structured payload as JSON so the text turn can read it (A-51).
            Some("data") => {
                if let Some(d) = p.get("data") {
                    pieces.push(render_data_part(d));
                }
            }
            // Unknown part kinds are ignored (tolerant, like the `extra` passthrough); if they were
            // the *only* content the emptiness check below refuses with `-32005`.
            _ => {}
        }
    }
    if pieces.is_empty() {
        return Err(error::CONTENT_TYPE_NOT_SUPPORTED);
    }
    Ok(pieces.join("\n"))
}

/// Render an A2A `data` part's structured payload as turn-input text (A-51): a labeled compact-JSON
/// block, so the agent can tell structured input apart from prose.
fn render_data_part(data: &Value) -> String {
    format!(
        "[structured data]\n{}",
        serde_json::to_string(data).unwrap_or_default()
    )
}

/// The client's requested `configuration.historyLength` (A2A, A-52): the cap on how many
/// most-recent conversation messages the returned `Task.history` should carry. Absent/invalid →
/// `None` (the server includes all retained history). Shared so every surface reads the cap
/// identically.
pub fn history_length(params: &Value) -> Option<usize> {
    params
        .get("configuration")?
        .get("historyLength")?
        .as_u64()
        .map(|n| n as usize)
}

/// Whether the client asked for a **blocking** send (`configuration.blocking == true`). The A2A
/// spec default is non-blocking — absent or `false` means "return a `submitted` task now and run
/// in the background" — so a stateful surface (A-54) branches on exactly this. Shared so every
/// surface reads the flag identically. (The reusable [`dispatch`] runner surface stays
/// synchronous-turn and does not consult it.)
pub fn blocking_requested(params: &Value) -> bool {
    params
        .get("configuration")
        .and_then(|c| c.get("blocking"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// The task id of a `tasks/*` request (`params.id`), if present. Shared by the stateful task
/// methods (`tasks/get`, `tasks/cancel`, `tasks/resubscribe`, `tasks/pushNotificationConfig/*`)
/// so id extraction cannot drift across them.
pub fn extract_task_id(params: &Value) -> Option<String> {
    params.get("id").and_then(Value::as_str).map(str::to_string)
}

/// `message/send`: extract the user's text, run one turn, and wrap the answer in a completed `Task`.
async fn send(
    runner: &dyn A2aTurn,
    realm: Option<&str>,
    id: Value,
    params: Option<&Value>,
) -> Value {
    let Some(params) = params else {
        return rpc_err(id, -32602, "missing params");
    };
    let input = match extract_input(params) {
        Ok(t) => t,
        Err(code) => return rpc_err(id, code, "message has no usable text or data part"),
    };
    // A-48: the conversation id crosses the seam, so a stateful runner can key one session per
    // `contextId` (and it is echoed on the task either way). A runner that only implements
    // `run`/`run_rich` keeps per-turn independence via the default `run_in_context`.
    let context_id = extract_context_id(params);
    let turn_ctx = A2aTurnContext {
        context_id: context_id.clone(),
        realm: realm.map(str::to_string),
    };
    match runner.run_in_context(&turn_ctx, &input).await {
        Ok(reply) => {
            // A-52: the answer text is the task's `status.message`; the runner's structured
            // (non-text) reply parts become `Task.artifacts` — the spec-faithful home for a task's
            // structured outputs (clients read artifacts first). A plain text answer yields none.
            let message = Message::agent_text(reply.text);
            let status = TaskStatus::new(TaskState::Completed, Some(message), Some(now_rfc3339()));
            let mut task = Task::new(new_id(), context_id, status);
            task.artifacts = artifacts_from_parts(reply.extra_parts);
            rpc_ok(id, serde_json::to_value(&task).unwrap_or(Value::Null))
        }
        Err(e) => rpc_err(id, -32603, format!("agent error: {e}")),
    }
}

/// Wrap a turn's structured (non-text) reply parts as a task's [`Artifact`]s (A-52). All extra
/// parts group into one artifact (a turn's structured output is one deliverable); a turn with no
/// extra parts produces no artifact, so a plain text answer stays `artifacts: []`.
fn artifacts_from_parts(parts: Vec<Part>) -> Vec<Artifact> {
    if parts.is_empty() {
        Vec::new()
    } else {
        vec![Artifact {
            artifact_id: Some(new_id()),
            parts,
            ..Default::default()
        }]
    }
}

// ── Message helpers ─────────────────────────────────────────────────────────────

/// Concatenate the text parts of an inbound A2A `message` (parts with `kind == "text"`).
pub fn extract_text(params: &Value) -> Option<String> {
    let parts = params.get("message")?.get("parts")?.as_array()?;
    let texts: Vec<&str> = parts
        .iter()
        .filter_map(|p| {
            if p.get("kind")?.as_str()? == "text" {
                p.get("text")?.as_str()
            } else {
                None
            }
        })
        .collect();
    (!texts.is_empty()).then(|| texts.join("\n"))
}

/// The conversation `contextId` the client supplied on the message, if any.
pub fn extract_context_id(params: &Value) -> Option<String> {
    params
        .get("message")?
        .get("contextId")?
        .as_str()
        .map(str::to_string)
}

// ── Streaming frame shaping ─────────────────────────────────────────────────────

/// Build the `result` value of a `message/stream` SSE frame: a serialized [`TaskStatusUpdateEvent`].
/// A streaming surface wraps it in a JSON-RPC response with the request id and emits it as an SSE
/// `data:` line (so the frame is a plain JSON-RPC response per spec).
pub fn status_update_value(
    task_id: &str,
    context_id: &str,
    state: TaskState,
    message: Option<Message>,
    is_final: bool,
) -> Value {
    let status = TaskStatus::new(state, message, Some(now_rfc3339()));
    let evt = TaskStatusUpdateEvent::new(task_id, Some(context_id.to_string()), status, is_final);
    serde_json::to_value(&evt).unwrap_or(Value::Null)
}

/// Build the `result` value of a `message/stream` SSE frame carrying a produced artifact: a
/// serialized [`TaskArtifactUpdateEvent`] (A-52). The reusable home for artifact framing, mirroring
/// [`status_update_value`] — a streaming surface that produces structured outputs wraps this in a
/// JSON-RPC response and emits it as an SSE `data:` line. `last_chunk` marks the final frame of a
/// (possibly chunked) artifact; `append` is left `false` (each frame is a whole artifact).
pub fn artifact_update_value(
    task_id: &str,
    context_id: &str,
    artifact: Artifact,
    last_chunk: bool,
) -> Value {
    let evt = TaskArtifactUpdateEvent {
        kind: "artifact-update".to_string(),
        task_id: task_id.to_string(),
        context_id: Some(context_id.to_string()),
        artifact,
        append: false,
        last_chunk,
    };
    serde_json::to_value(&evt).unwrap_or(Value::Null)
}

// ── JSON-RPC envelopes ──────────────────────────────────────────────────────────

/// A JSON-RPC 2.0 success envelope: `{ jsonrpc, id, result }`.
pub fn rpc_ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// A JSON-RPC 2.0 error envelope: `{ jsonrpc, id, error: { code, message } }`.
pub fn rpc_err(id: Value, code: i32, msg: impl Into<String>) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": msg.into() } })
}

// ── Timestamp ───────────────────────────────────────────────────────────────────

/// ISO 8601 / RFC 3339 UTC timestamp from [`std::time::SystemTime`] — no external deps.
/// Uses Howard Hinnant's civil-from-days algorithm (2013).
pub fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    rfc3339_secs(secs)
}

/// RFC 3339 UTC timestamp from milliseconds since the Unix epoch — the event store's clock
/// convention — so a projected task's `status.timestamp` can reflect *when the state was reached*
/// (the stored event's `ts`), not when it was read (A-54). Sub-second precision is dropped: the
/// A2A timestamps are whole-second, matching [`now_rfc3339`].
pub fn rfc3339_ms(ms: i64) -> String {
    rfc3339_secs(ms.div_euclid(1000))
}

fn rfc3339_secs(secs: i64) -> String {
    // Euclidean arithmetic throughout so a pre-epoch (negative) input still formats a valid
    // timestamp — identical to truncating division for every non-negative input.
    let s = secs.rem_euclid(60);
    let min = secs.div_euclid(60).rem_euclid(60);
    let h = secs.div_euclid(3_600).rem_euclid(24);
    // civil_from_days
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097; // 0 <= doe < 146097
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{min:02}:{s:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocking_requested_defaults_to_non_blocking() {
        // The A2A spec default: absent configuration (or absent/false flag) = non-blocking.
        assert!(!blocking_requested(&json!({ "message": {} })));
        assert!(!blocking_requested(
            &json!({ "configuration": { "historyLength": 3 } })
        ));
        assert!(!blocking_requested(
            &json!({ "configuration": { "blocking": false } })
        ));
        assert!(blocking_requested(
            &json!({ "configuration": { "blocking": true } })
        ));
        // A non-boolean value is treated as absent, never as true.
        assert!(!blocking_requested(
            &json!({ "configuration": { "blocking": "yes" } })
        ));
    }

    #[test]
    fn extract_task_id_reads_params_id() {
        assert_eq!(
            extract_task_id(&json!({ "id": "s_42" })).as_deref(),
            Some("s_42")
        );
        assert!(extract_task_id(&json!({ "taskId": "s_42" })).is_none());
        assert!(extract_task_id(&json!({ "id": 42 })).is_none());
    }

    #[test]
    fn rfc3339_ms_matches_the_seconds_formatter_and_truncates() {
        // 2026-01-02T03:04:05.678Z → the millisecond part is dropped, not rounded.
        let ms = 1_767_323_045_678i64;
        assert_eq!(rfc3339_ms(ms), "2026-01-02T03:04:05Z");
        // Epoch and a negative (pre-epoch) value stay well-formed via div_euclid.
        assert_eq!(rfc3339_ms(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_ms(-1), "1969-12-31T23:59:59Z");
    }

    #[test]
    fn card_url_defaults_to_http_without_forwarded_proto() {
        assert_eq!(
            card_url(None, "example.com", "/a2a"),
            "http://example.com/a2a"
        );
    }

    #[test]
    fn card_url_honors_forwarded_proto_when_present() {
        assert_eq!(
            card_url(Some("https"), "example.com", "/agent/a2a"),
            "https://example.com/agent/a2a"
        );
    }

    #[test]
    fn card_url_joins_the_path_verbatim() {
        // No inserted separator beyond what the caller's `path` already carries — a caller that
        // forgets the leading `/` gets a malformed URL back, same as the inlined derivation did.
        assert_eq!(
            card_url(
                Some("http"),
                "localhost:8080",
                "/.well-known/agent-card.json"
            ),
            "http://localhost:8080/.well-known/agent-card.json"
        );
    }

    /// A canned runner: echoes the input as the answer — no model, no key.
    struct StubRunner;

    #[async_trait]
    impl A2aTurn for StubRunner {
        async fn run(&self, input: &str) -> Result<String, String> {
            Ok(format!("you said: {input}"))
        }
    }

    /// A runner that always fails — exercises the JSON-RPC error path.
    struct FailRunner;

    #[async_trait]
    impl A2aTurn for FailRunner {
        async fn run(&self, _input: &str) -> Result<String, String> {
            Err("boom".to_string())
        }
    }

    fn send_body(text: &str) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "message/send",
            "params": { "message": {
                "kind": "message", "messageId": "m1", "role": "user",
                "parts": [{ "kind": "text", "text": text }],
            }},
        })
    }

    /// A runner that attaches a typed `data` part beside its text — the rich-reply seam.
    struct BlockRunner;

    #[async_trait]
    impl A2aTurn for BlockRunner {
        async fn run(&self, input: &str) -> Result<String, String> {
            Ok(format!("you said: {input}"))
        }
        async fn run_rich(&self, input: &str) -> Result<A2aReply, String> {
            Ok(A2aReply {
                text: self.run(input).await?,
                extra_parts: vec![Part::data(
                    json!({ "block": { "type": "table", "rows": [{ "a": 1 }] } }),
                )],
            })
        }
    }

    /// A-52 (failing-first): a runner that produces structured (non-text) reply parts surfaces them
    /// as `Task.artifacts` — the spec-faithful home for a task's structured outputs — while the
    /// answer text stays in `status.message`. (Supersedes the earlier "data parts ride beside the
    /// answer text in the message" shaping; the default text-only runner is unaffected — see the
    /// sibling `message_send_*` test asserting `artifacts: []`.)
    #[tokio::test]
    async fn rich_replies_surface_data_parts_as_artifacts() {
        let resp = dispatch(&BlockRunner, None, &send_body("hello")).await;
        // The message carries only the answer text (no extra parts hitchhiking on it).
        let parts = &resp["result"]["status"]["message"]["parts"];
        assert_eq!(parts[0]["kind"], "text");
        assert_eq!(parts[0]["text"], "you said: hello");
        assert!(parts[1].is_null(), "no extra parts on the message: {resp}");
        // The structured `data` part is a task artifact.
        let artifacts = resp["result"]["artifacts"]
            .as_array()
            .expect("artifacts array");
        assert_eq!(artifacts.len(), 1, "one artifact: {resp}");
        assert!(
            artifacts[0]["artifactId"].as_str().is_some(),
            "artifact has an id: {resp}"
        );
        let apart = &artifacts[0]["parts"][0];
        assert_eq!(apart["kind"], "data");
        assert_eq!(apart["data"]["block"]["type"], "table");
        assert_eq!(apart["data"]["block"]["rows"][0]["a"], 1);
    }

    #[tokio::test]
    async fn message_send_returns_a_completed_task_with_the_answer() {
        let resp = dispatch(&StubRunner, None, &send_body("hello")).await;
        let result = &resp["result"];
        assert_eq!(result["kind"], "task");
        assert_eq!(result["status"]["state"], "completed");
        assert_eq!(
            result["status"]["message"]["parts"][0]["text"],
            "you said: hello"
        );
        // The completed status carries a timestamp.
        assert!(result["status"]["timestamp"].as_str().is_some());
        // A plain text answer produces no artifacts (A-52).
        assert!(
            result["artifacts"].as_array().is_some_and(|a| a.is_empty()),
            "text-only answer has empty artifacts: {result}"
        );
    }

    #[tokio::test]
    async fn unknown_method_is_method_not_found() {
        let body = json!({ "jsonrpc": "2.0", "id": 2, "method": "tasks/send", "params": {} });
        let resp = dispatch(&StubRunner, None, &body).await;
        assert_eq!(resp["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn empty_message_parts_is_a_param_error() {
        let body = json!({
            "jsonrpc": "2.0", "id": 3, "method": "message/send",
            "params": { "message": { "parts": [] } },
        });
        let resp = dispatch(&StubRunner, None, &body).await;
        assert_eq!(resp["error"]["code"], -32602);
    }

    #[tokio::test]
    async fn bad_jsonrpc_version_is_invalid_request() {
        let body = json!({ "jsonrpc": "1.0", "id": 4, "method": "message/send" });
        let resp = dispatch(&StubRunner, None, &body).await;
        assert_eq!(resp["error"]["code"], -32600);
    }

    #[tokio::test]
    async fn runner_error_is_internal_error() {
        let resp = dispatch(&FailRunner, None, &send_body("hi")).await;
        assert_eq!(resp["error"]["code"], -32603);
    }

    #[test]
    fn card_lists_skills_and_carries_streaming_flag() {
        let card = agent_card(
            "support",
            "Answer from the FAQ.",
            Some("http://h/support/a2a".to_string()),
            "9.9.9",
            &[(
                "search".to_string(),
                "FAQ Search".to_string(),
                "Search the FAQ knowledge base.".to_string(),
            )],
            false,
        );
        assert_eq!(card.name, "support");
        assert_eq!(card.url.as_deref(), Some("http://h/support/a2a"));
        assert_eq!(card.version, "9.9.9");
        assert!(!card.capabilities.streaming);
        // The id and the human name are preserved independently.
        let skill = card.skills.iter().find(|s| s.id == "search").unwrap();
        assert_eq!(skill.name, "FAQ Search");
    }

    /// A-49 (failing-first): a served card carries the spec-required `protocolVersion`, a non-empty
    /// `interfaces` whose JSON-RPC entry's url equals the card `url`, and a matching
    /// `preferredTransport`; `supportsAuthenticatedExtendedCard` is an honest `false`. The
    /// `rpc_endpoint()` resolver still resolves against the now-populated interfaces.
    #[test]
    fn card_declares_protocol_version_interface_and_preferred_transport() {
        let url = "http://h/support/a2a";
        let card = agent_card(
            "support",
            "Answer from the FAQ.",
            Some(url.to_string()),
            "9.9.9",
            &[],
            true,
        );
        assert_eq!(card.protocol_version, PROTOCOL_VERSION);
        assert!(
            !card.protocol_version.is_empty(),
            "protocolVersion is required"
        );
        assert_eq!(card.preferred_transport.as_deref(), Some(TRANSPORT_JSONRPC));
        assert_eq!(card.interfaces.len(), 1, "one declared transport interface");
        assert_eq!(
            card.interfaces[0].transport.as_deref(),
            Some(TRANSPORT_JSONRPC)
        );
        assert_eq!(
            card.interfaces[0].url.as_deref(),
            Some(url),
            "the JSON-RPC interface url equals the card url"
        );
        assert_eq!(card.supports_authenticated_extended_card, Some(false));
        // The resolver still works now that the interfaces path is live (it prefers `url`).
        assert_eq!(card.rpc_endpoint().as_deref(), Some(url));

        // Serializes under the spec's camelCase keys.
        let v = serde_json::to_value(&card).unwrap();
        assert_eq!(v["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(v["preferredTransport"], TRANSPORT_JSONRPC);
        assert_eq!(v["interfaces"][0]["transport"], TRANSPORT_JSONRPC);
        assert_eq!(v["interfaces"][0]["url"], url);
        assert_eq!(v["supportsAuthenticatedExtendedCard"], false);
    }

    /// A card built without a `url` (degenerate/never-served) declares no transport — an honest
    /// empty state, not a `preferredTransport` pointing at nothing.
    #[test]
    fn card_without_url_declares_no_transport() {
        let card = agent_card("x", "d", None, "1.0.0", &[], false);
        assert!(card.interfaces.is_empty());
        assert!(card.preferred_transport.is_none());
        assert_eq!(
            card.protocol_version, PROTOCOL_VERSION,
            "still spec-versioned"
        );
    }

    /// A-50 (failing-first): a defined-but-unsupported A2A method dispatches to `-32004`
    /// UnsupportedOperation, a genuinely-unknown name keeps `-32601`, and `is_unsupported_a2a_method`
    /// is the shared classifier both dispatch sites use (so they cannot drift).
    #[tokio::test]
    async fn unsupported_a2a_method_returns_unsupported_operation() {
        let body =
            |method: &str| json!({ "jsonrpc": "2.0", "id": 7, "method": method, "params": {} });
        for method in [
            "tasks/cancel",
            "tasks/resubscribe",
            "tasks/pushNotificationConfig/set",
            "tasks/pushNotificationConfig/get",
            "tasks/pushNotificationConfig/list",
            "tasks/pushNotificationConfig/delete",
            "agent/getAuthenticatedExtendedCard",
        ] {
            assert!(
                is_unsupported_a2a_method(method),
                "{method} must be classified"
            );
            let resp = dispatch(&StubRunner, None, &body(method)).await;
            assert_eq!(
                resp["error"]["code"],
                error::UNSUPPORTED_OPERATION,
                "{method} → -32004: {resp}"
            );
        }
        // A genuinely-unrecognized method name is still -32601 (correct JSON-RPC).
        assert!(!is_unsupported_a2a_method("foo/bar"));
        let resp = dispatch(&StubRunner, None, &body("foo/bar")).await;
        assert_eq!(resp["error"]["code"], -32601, "{resp}");
    }

    /// A-50 (failing-first): a `message/send` whose message carries a part but no text part is
    /// refused with `-32005` ContentTypeNotSupported (rather than running an empty turn), while an
    /// empty/absent parts array stays `-32602` invalid params.
    #[tokio::test]
    async fn no_usable_text_is_content_type_not_supported() {
        let file_only = json!({
            "jsonrpc": "2.0", "id": 8, "method": "message/send",
            "params": { "message": { "parts": [{ "kind": "file", "file": {} }] } },
        });
        let resp = dispatch(&StubRunner, None, &file_only).await;
        assert_eq!(
            resp["error"]["code"],
            error::CONTENT_TYPE_NOT_SUPPORTED,
            "file-only message → -32005: {resp}"
        );

        // Empty parts is a malformed request, not an unusable content type → -32602.
        let empty = json!({
            "jsonrpc": "2.0", "id": 9, "method": "message/send",
            "params": { "message": { "parts": [] } },
        });
        let resp = dispatch(&StubRunner, None, &empty).await;
        assert_eq!(resp["error"]["code"], -32602, "{resp}");
    }

    #[test]
    fn extract_text_joins_text_parts_and_ignores_others() {
        let params = json!({
            "message": { "parts": [
                { "kind": "text", "text": "hello" },
                { "kind": "file", "file": {} },
                { "kind": "text", "text": "flux" },
            ]}
        });
        assert_eq!(extract_text(&params).as_deref(), Some("hello\nflux"));

        let empty = json!({ "message": { "parts": [] } });
        assert!(extract_text(&empty).is_none());
    }

    #[test]
    fn extract_context_id_reads_message() {
        let params = json!({ "message": { "contextId": "ctx-7", "parts": [] } });
        assert_eq!(extract_context_id(&params).as_deref(), Some("ctx-7"));
        let none = json!({ "message": { "parts": [] } });
        assert!(extract_context_id(&none).is_none());
    }

    /// A-51 (failing-first): `extract_input` composes text parts and surfaces `data` parts as their
    /// structured JSON, so a message carrying a `data` part runs a real turn (not an empty one).
    #[test]
    fn extract_input_composes_text_and_surfaces_data_parts() {
        let params = json!({ "message": { "parts": [
            { "kind": "text", "text": "look:" },
            { "kind": "data", "data": { "temp": 21, "unit": "C" } },
        ]}});
        let input = extract_input(&params).expect("text+data is usable");
        assert!(input.contains("look:"), "text surfaced: {input}");
        assert!(
            input.contains("\"temp\":21"),
            "data payload surfaced: {input}"
        );

        // A data-only message is no longer an empty turn — the payload is the input.
        let data_only = json!({ "message": { "parts": [{ "kind": "data", "data": { "x": 1 } }] }});
        assert!(extract_input(&data_only).unwrap().contains("\"x\":1"));
    }

    /// A-51 (failing-first): a `file` part is refused with `-32005` (flux's text turn can't consume
    /// it) even alongside text — never silently dropped; an empty/absent parts array stays `-32602`.
    #[test]
    fn extract_input_refuses_files_and_malformed() {
        let with_file = json!({ "message": { "parts": [
            { "kind": "text", "text": "hi" },
            { "kind": "file", "file": { "uri": "http://x/y.png" } },
        ]}});
        assert_eq!(
            extract_input(&with_file),
            Err(error::CONTENT_TYPE_NOT_SUPPORTED),
            "a file part refuses the whole message"
        );
        assert_eq!(
            extract_input(&json!({ "message": { "parts": [] }})),
            Err(-32602)
        );
        assert_eq!(extract_input(&json!({ "params": {} })), Err(-32602));
    }

    #[test]
    fn history_length_reads_configuration() {
        let p = json!({ "configuration": { "historyLength": 5 }, "message": {} });
        assert_eq!(history_length(&p), Some(5));
        assert_eq!(history_length(&json!({ "message": {} })), None);
    }

    /// A-52 (failing-first): the reusable artifact-update frame shaper carries the produced
    /// artifact under the spec's `artifact-update` kind + camelCase keys.
    #[test]
    fn artifact_update_value_shapes_a_frame() {
        let artifact = Artifact {
            artifact_id: Some("a1".to_string()),
            parts: vec![Part::data(json!({ "k": "v" }))],
            ..Default::default()
        };
        let v = artifact_update_value("task-1", "ctx-1", artifact, true);
        assert_eq!(v["kind"], "artifact-update");
        assert_eq!(v["taskId"], "task-1");
        assert_eq!(v["contextId"], "ctx-1");
        assert_eq!(v["artifact"]["artifactId"], "a1");
        assert_eq!(v["artifact"]["parts"][0]["kind"], "data");
        assert_eq!(v["lastChunk"], true);
    }

    /// A-48 (failing-first acceptance): the conversation id crosses the seam — a STATEFUL runner
    /// keyed on `context_id` accumulates turns across two `dispatch` calls (and sees `None` when
    /// the client sent no id). Legacy runners (StubRunner &co above) keep compiling and behaving
    /// per-turn via the default `run_in_context`.
    #[tokio::test]
    async fn dispatch_passes_context_so_a_stateful_runner_accumulates() {
        use std::collections::HashMap;
        use std::sync::Mutex;

        struct StatefulRunner {
            history: Mutex<HashMap<String, Vec<String>>>,
        }

        #[async_trait]
        impl A2aTurn for StatefulRunner {
            async fn run(&self, _input: &str) -> Result<String, String> {
                Err("stateful runner must be driven through run_in_context".into())
            }
            async fn run_in_context(
                &self,
                ctx: &A2aTurnContext,
                input: &str,
            ) -> Result<A2aReply, String> {
                let key = ctx.context_id.clone().unwrap_or_else(|| "<none>".into());
                let mut h = self.history.lock().unwrap();
                let turns = h.entry(key).or_default();
                turns.push(input.to_string());
                Ok(A2aReply {
                    text: format!("turns:{}", turns.len()),
                    extra_parts: Vec::new(),
                })
            }
        }

        fn body_with_ctx(text: &str, ctx: &str) -> Value {
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "message/send",
                "params": { "message": {
                    "kind": "message", "messageId": "m1", "role": "user",
                    "contextId": ctx,
                    "parts": [{ "kind": "text", "text": text }],
                }},
            })
        }

        let runner = StatefulRunner {
            history: Mutex::new(HashMap::new()),
        };
        let answer = |v: &Value| {
            v["result"]["status"]["message"]["parts"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        };

        let r1 = dispatch(&runner, None, &body_with_ctx("one", "ctx-s")).await;
        let r2 = dispatch(&runner, None, &body_with_ctx("two", "ctx-s")).await;
        let other = dispatch(&runner, None, &body_with_ctx("hi", "ctx-other")).await;
        assert_eq!(answer(&r1), "turns:1");
        assert_eq!(answer(&r2), "turns:2", "same contextId accumulates: {r2}");
        assert_eq!(
            answer(&other),
            "turns:1",
            "a different contextId is isolated"
        );
    }

    /// D-69 (failing-first acceptance): the mount's authenticated realm crosses the seam as an
    /// explicit `dispatch` parameter and lands on [`A2aTurnContext::realm`] — while the
    /// `contextId` extraction from the body stays exactly as before.
    #[tokio::test]
    async fn dispatch_threads_the_authenticated_realm_into_the_turn_context() {
        use std::sync::Mutex;

        struct CaptureRunner {
            seen: Mutex<Vec<A2aTurnContext>>,
        }

        #[async_trait]
        impl A2aTurn for CaptureRunner {
            async fn run(&self, _input: &str) -> Result<String, String> {
                Err("capture runner must be driven through run_in_context".into())
            }
            async fn run_in_context(
                &self,
                ctx: &A2aTurnContext,
                _input: &str,
            ) -> Result<A2aReply, String> {
                self.seen.lock().unwrap().push(ctx.clone());
                Ok(A2aReply {
                    text: "ok".to_string(),
                    extra_parts: Vec::new(),
                })
            }
        }

        let runner = CaptureRunner {
            seen: Mutex::new(Vec::new()),
        };
        let body = json!({
            "jsonrpc": "2.0", "id": 1, "method": "message/send",
            "params": { "message": {
                "kind": "message", "messageId": "m1", "role": "user",
                "contextId": "ctx-9",
                "parts": [{ "kind": "text", "text": "hi" }],
            }},
        });

        dispatch(&runner, Some("acme"), &body).await;
        dispatch(&runner, None, &body).await;

        let seen = runner.seen.lock().unwrap();
        assert_eq!(seen[0].realm.as_deref(), Some("acme"));
        assert_eq!(
            seen[0].context_id.as_deref(),
            Some("ctx-9"),
            "contextId extraction is unchanged"
        );
        assert_eq!(seen[1].realm, None, "unauthenticated mount passes None");
        assert_eq!(seen[1].context_id.as_deref(), Some("ctx-9"));
    }

    #[test]
    fn status_update_value_shapes_a_stream_frame() {
        let v = status_update_value(
            "task-9",
            "ctx-1",
            TaskState::Working,
            Some(Message::agent_text("thinking…")),
            false,
        );
        assert_eq!(v["kind"], "status-update");
        assert_eq!(v["taskId"], "task-9");
        assert_eq!(v["contextId"], "ctx-1");
        assert_eq!(v["status"]["state"], "working");
        assert_eq!(v["final"], false);
    }

    #[test]
    fn now_rfc3339_looks_valid() {
        let ts = now_rfc3339();
        assert_eq!(ts.len(), 20, "unexpected length: {ts}");
        assert!(ts.ends_with('Z'), "must end with Z: {ts}");
        assert!(ts.contains('T'), "must contain T: {ts}");
        let year: u32 = ts[..4].parse().unwrap();
        assert!(year >= 2024, "year looks wrong: {ts}");
    }
}
