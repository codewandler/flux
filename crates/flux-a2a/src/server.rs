//! The reusable **server** side of the A2A protocol: JSON-RPC dispatch, the agent-card builder, and
//! the message/event shaping — all over [`serde_json::Value`] and a small [`A2aTurn`] seam, with
//! **no HTTP-framework dependency**. A surface (axum in `flux-server`, or another downstream HTTP
//! host) supplies the route + state and calls these functions, so the *protocol* has one
//! definition the way [`crate::types`] gives the *wire* one definition.
//!
//! The blocking [`dispatch`] handles `message/send` end-to-end (run a turn → completed [`Task`]).
//! `message/stream` (SSE) is not handled here — it needs the surface's streaming machinery — but the
//! frame shaping ([`status_update_value`]) and the pure helpers ([`extract_text`],
//! [`extract_context_id`], [`agent_card`], [`now_rfc3339`], [`rpc_ok`]/[`rpc_err`]) are shared so a
//! streaming surface re-uses them too.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::types::{
    new_id, AgentCard, Capabilities, Message, Part, Skill, Task, TaskState, TaskStatus,
    TaskStatusUpdateEvent,
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
/// [`dispatch`] does not).
pub fn agent_card(
    name: &str,
    description: &str,
    url: Option<String>,
    version: &str,
    skills: &[(String, String, String)],
    streaming: bool,
) -> AgentCard {
    AgentCard {
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
        interfaces: Vec::new(),
    }
}

// ── JSON-RPC dispatch (blocking methods) ────────────────────────────────────────

/// Handle one JSON-RPC 2.0 request body for the **blocking** A2A methods, running `message/send`
/// through `runner`. Returns the JSON-RPC response value: success carries a completed [`Task`] (the
/// answer in `status.message`); an unknown method or bad params carry a JSON-RPC error.
pub async fn dispatch(runner: &dyn A2aTurn, body: &Value) -> Value {
    let id = body.get("id").cloned().unwrap_or(Value::Null);
    if body.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return rpc_err(id, -32600, "jsonrpc must be \"2.0\"");
    }
    match body
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "message/send" => send(runner, id, body.get("params")).await,
        other => rpc_err(id, -32601, format!("method not found: {other}")),
    }
}

/// `message/send`: extract the user's text, run one turn, and wrap the answer in a completed `Task`.
async fn send(runner: &dyn A2aTurn, id: Value, params: Option<&Value>) -> Value {
    let Some(params) = params else {
        return rpc_err(id, -32602, "missing params");
    };
    let Some(input) = extract_text(params) else {
        return rpc_err(id, -32602, "no text found in message parts");
    };
    // A-48: the conversation id crosses the seam, so a stateful runner can key one session per
    // `contextId` (and it is echoed on the task either way). A runner that only implements
    // `run`/`run_rich` keeps per-turn independence via the default `run_in_context`.
    let context_id = extract_context_id(params);
    let turn_ctx = A2aTurnContext {
        context_id: context_id.clone(),
    };
    match runner.run_in_context(&turn_ctx, &input).await {
        Ok(reply) => {
            let mut message = Message::agent_text(reply.text);
            message.parts.extend(reply.extra_parts);
            let status = TaskStatus::new(TaskState::Completed, Some(message), Some(now_rfc3339()));
            let task = Task::new(new_id(), context_id, status);
            rpc_ok(id, serde_json::to_value(&task).unwrap_or(Value::Null))
        }
        Err(e) => rpc_err(id, -32603, format!("agent error: {e}")),
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
    let s = secs % 60;
    let min = (secs / 60) % 60;
    let h = (secs / 3_600) % 24;
    // civil_from_days
    let z = secs / 86_400 + 719_468;
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

    /// `run_rich` extra parts ride the reply message beside the text part; the default impl
    /// (StubRunner above, exercised by the sibling test) keeps text-only runners unchanged.
    #[tokio::test]
    async fn rich_replies_attach_data_parts_beside_the_text() {
        let resp = dispatch(&BlockRunner, &send_body("hello")).await;
        let parts = &resp["result"]["status"]["message"]["parts"];
        assert_eq!(parts[0]["kind"], "text");
        assert_eq!(parts[0]["text"], "you said: hello");
        assert_eq!(parts[1]["kind"], "data");
        assert_eq!(parts[1]["data"]["block"]["type"], "table");
        assert_eq!(parts[1]["data"]["block"]["rows"][0]["a"], 1);
    }

    #[tokio::test]
    async fn message_send_returns_a_completed_task_with_the_answer() {
        let resp = dispatch(&StubRunner, &send_body("hello")).await;
        let result = &resp["result"];
        assert_eq!(result["kind"], "task");
        assert_eq!(result["status"]["state"], "completed");
        assert_eq!(
            result["status"]["message"]["parts"][0]["text"],
            "you said: hello"
        );
        // The completed status carries a timestamp.
        assert!(result["status"]["timestamp"].as_str().is_some());
    }

    #[tokio::test]
    async fn unknown_method_is_method_not_found() {
        let body = json!({ "jsonrpc": "2.0", "id": 2, "method": "tasks/send", "params": {} });
        let resp = dispatch(&StubRunner, &body).await;
        assert_eq!(resp["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn empty_message_parts_is_a_param_error() {
        let body = json!({
            "jsonrpc": "2.0", "id": 3, "method": "message/send",
            "params": { "message": { "parts": [] } },
        });
        let resp = dispatch(&StubRunner, &body).await;
        assert_eq!(resp["error"]["code"], -32602);
    }

    #[tokio::test]
    async fn bad_jsonrpc_version_is_invalid_request() {
        let body = json!({ "jsonrpc": "1.0", "id": 4, "method": "message/send" });
        let resp = dispatch(&StubRunner, &body).await;
        assert_eq!(resp["error"]["code"], -32600);
    }

    #[tokio::test]
    async fn runner_error_is_internal_error() {
        let resp = dispatch(&FailRunner, &send_body("hi")).await;
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

        let r1 = dispatch(&runner, &body_with_ctx("one", "ctx-s")).await;
        let r2 = dispatch(&runner, &body_with_ctx("two", "ctx-s")).await;
        let other = dispatch(&runner, &body_with_ctx("hi", "ctx-other")).await;
        assert_eq!(answer(&r1), "turns:1");
        assert_eq!(answer(&r2), "turns:2", "same contextId accumulates: {r2}");
        assert_eq!(
            answer(&other),
            "turns:1",
            "a different contextId is isolated"
        );
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
