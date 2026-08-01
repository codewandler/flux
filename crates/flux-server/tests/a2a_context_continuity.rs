//! A-48: stateful A2A mode — one session per `contextId`. A request whose `contextId` matches a
//! live A2A session CONTINUES it (the engine's conversation projection provides multi-turn
//! memory); a fresh/absent `contextId` keeps per-task isolation. Driven through the real
//! production router end to end, with a provider whose answer *proves* what conversation it saw.

mod support;

use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::json;

/// Answers `seen:<N>` where `N` is the number of USER messages in the request — a memory probe:
/// a continued session shows a growing count; an isolated one always shows 1.
struct MemoryProbeProvider;

#[async_trait::async_trait]
impl flux_provider::Provider for MemoryProbeProvider {
    fn name(&self) -> &str {
        "mock"
    }
    async fn stream(
        &self,
        req: flux_provider::Request,
    ) -> flux_core::Result<flux_provider::ChunkStream> {
        if req.tools.iter().any(|tool| tool.name == "declare_intent") {
            return Ok(Box::pin(futures::stream::iter(vec![
                Ok(flux_core::Chunk::Block(flux_core::ContentBlock::ToolUse {
                    id: "intent".into(),
                    name: "declare_intent".into(),
                    input: json!({
                        "intent": "report conversation memory",
                        "capability_families": [],
                    }),
                })),
                Ok(flux_core::Chunk::Done {
                    stop_reason: Some(flux_core::StopReason::ToolUse),
                }),
            ])));
        }
        let users = req
            .messages
            .iter()
            .filter(|m| m.role == flux_core::Role::User)
            .count();
        Ok(Box::pin(futures::stream::iter(vec![
            Ok(flux_core::Chunk::TextDelta(format!("seen:{users}"))),
            Ok(flux_core::Chunk::Done {
                stop_reason: Some(flux_core::StopReason::EndTurn),
            }),
        ])))
    }
}

fn send_body(context_id: Option<&str>, text: &str, id: u64) -> serde_json::Value {
    let mut message = json!({ "parts": [{ "kind": "text", "text": text }] });
    if let Some(cid) = context_id {
        message["contextId"] = json!(cid);
    }
    // Blocking send: these tests assert the synchronous completed-Task shape (A-54 makes
    // non-blocking the default).
    json!({
        "jsonrpc": "2.0", "id": id, "method": "message/send",
        "params": { "message": message, "configuration": { "blocking": true } },
    })
}

fn answer(task: &serde_json::Value) -> String {
    task["result"]["status"]["message"]["parts"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn task_id(task: &serde_json::Value) -> String {
    task["result"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The acceptance centerpiece: two `message/send` calls with the SAME `contextId` continue one
/// session — same task id, and the second answer proves the model saw the first turn's message.
#[tokio::test]
async fn same_context_id_continues_the_session_with_memory() {
    let engine = support::test_engine(Arc::new(MemoryProbeProvider));
    let app = flux_server::router_in(
        engine,
        flux_server::ServerAuth::Open,
        flux_server::CardInfo::flux_coding(),
        "127.0.0.1:0".parse().unwrap(),
        &support::pinned_env(),
    )
    .unwrap();

    let (s1, r1) =
        support::post_json(app.clone(), "/a2a", send_body(Some("ctx-mem"), "one", 1)).await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(
        answer(&r1),
        "seen:1",
        "first turn sees one user message: {r1}"
    );

    let (s2, r2) = support::post_json(app, "/a2a", send_body(Some("ctx-mem"), "two", 2)).await;
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(
        answer(&r2),
        "seen:2",
        "the second turn CONTINUES the conversation (memory of turn one): {r2}"
    );
    assert_eq!(
        task_id(&r1),
        task_id(&r2),
        "one session per contextId — the task id is stable across the conversation"
    );
}

/// Different `contextId`s stay isolated; a request without one keeps per-task isolation.
#[tokio::test]
async fn different_or_absent_context_ids_stay_isolated() {
    let engine = support::test_engine(Arc::new(MemoryProbeProvider));
    let app = flux_server::router_in(
        engine,
        flux_server::ServerAuth::Open,
        flux_server::CardInfo::flux_coding(),
        "127.0.0.1:0".parse().unwrap(),
        &support::pinned_env(),
    )
    .unwrap();

    let (_, a1) = support::post_json(app.clone(), "/a2a", send_body(Some("ctx-a"), "hi", 1)).await;
    let (_, b1) = support::post_json(app.clone(), "/a2a", send_body(Some("ctx-b"), "hi", 2)).await;
    assert_eq!(answer(&a1), "seen:1");
    assert_eq!(
        answer(&b1),
        "seen:1",
        "a different contextId never sees ctx-a's turn"
    );
    assert_ne!(
        task_id(&a1),
        task_id(&b1),
        "distinct conversations, distinct sessions"
    );

    let (_, n1) = support::post_json(app.clone(), "/a2a", send_body(None, "hi", 3)).await;
    let (_, n2) = support::post_json(app, "/a2a", send_body(None, "hi", 4)).await;
    assert_eq!(answer(&n1), "seen:1");
    assert_eq!(
        answer(&n2),
        "seen:1",
        "no contextId → per-task isolation, exactly as before"
    );
    assert_ne!(task_id(&n1), task_id(&n2));
}
