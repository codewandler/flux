//! `flux-a2a` — the A2A (Agent-to-Agent) protocol: spec-conformant wire types plus an HTTP +
//! JSON-RPC 2.0 client for driving a remote A2A agent.
//!
//! Two halves:
//! - [`types`] — the wire objects ([`Message`]/[`Part`], [`Task`]/[`TaskStatus`], the streaming
//!   [`TaskStatusUpdateEvent`], [`AgentCard`], and the JSON-RPC envelope). These are shared by both
//!   the client here and `flux-server`'s A2A endpoint, so the wire has a single definition.
//! - [`client`] — [`A2aClient`]: discover (`/.well-known/agent-card.json`), then `message/send`
//!   (blocking) or `message/stream` (SSE) per turn, with `tasks/get` for async completion.
//!
//! The model is a **thin client**: one user turn maps to one remote A2A task. The remote agent runs
//! its own loop (model + tools); this crate just speaks the protocol.
//!
//! - [`server`] — the reusable, transport-agnostic server side ([`server::dispatch`],
//!   [`server::agent_card`], the [`server::A2aTurn`] seam, and the message/event shaping). A surface
//!   (axum in `flux-server` or another downstream HTTP host) provides the route + state and calls these.

mod client;
pub mod server;
pub mod types;

pub use client::{A2aClient, A2aError, EventStream, Result};
pub use types::{
    new_id, AgentCard, AgentInterface, Artifact, Capabilities, JsonRpcError, JsonRpcRequest,
    JsonRpcResponse, Message, Part, Role, SendConfiguration, SendMessageParams, SendOutcome, Skill,
    StreamEvent, Task, TaskArtifactUpdateEvent, TaskGetParams, TaskState, TaskStatus,
    TaskStatusUpdateEvent,
};

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    #[test]
    fn message_send_params_round_trip() {
        let msg = Message::user_text("hello flux", Some("ctx-1".to_string()));
        let params = SendMessageParams {
            message: msg,
            configuration: Some(SendConfiguration { blocking: true }),
        };
        let v = serde_json::to_value(&params).unwrap();
        // Message is nested under `message`; parts carry a `kind` discriminator; config blocking.
        assert_eq!(v["message"]["kind"], "message");
        assert_eq!(v["message"]["role"], "user");
        assert_eq!(v["message"]["parts"][0]["kind"], "text");
        assert_eq!(v["message"]["parts"][0]["text"], "hello flux");
        assert_eq!(v["message"]["contextId"], "ctx-1");
        assert_eq!(v["configuration"]["blocking"], true);
        assert!(v["message"]["messageId"].as_str().is_some());
    }

    #[test]
    fn status_update_sse_frame_decodes() {
        // A `message/stream` SSE frame is a full JSON-RPC response wrapping a status-update event.
        let frame = json!({
            "jsonrpc": "2.0",
            "id": "1",
            "result": {
                "kind": "status-update",
                "taskId": "task-9",
                "contextId": "ctx-1",
                "status": {
                    "state": "working",
                    "message": {
                        "kind": "message",
                        "messageId": "m1",
                        "role": "agent",
                        "parts": [{ "kind": "text", "text": "thinking…" }]
                    }
                },
                "final": false
            }
        });
        let env: JsonRpcResponse<Value> = serde_json::from_value(frame).unwrap();
        let ev = StreamEvent::from_value(env.result.unwrap()).unwrap();
        match ev {
            StreamEvent::StatusUpdate(u) => {
                assert_eq!(u.task_id, "task-9");
                assert!(!u.is_final);
                assert_eq!(u.status.state, TaskState::Working);
                assert_eq!(u.status.message.unwrap().text(), "thinking…");
            }
            other => panic!("expected StatusUpdate, got {other:?}"),
        }
    }

    #[test]
    fn final_event_state_and_terminal() {
        let v = json!({
            "kind": "status-update",
            "taskId": "t",
            "status": { "state": "completed" },
            "final": true
        });
        match StreamEvent::from_value(v).unwrap() {
            StreamEvent::StatusUpdate(u) => {
                assert!(u.is_final);
                assert!(u.status.state.is_terminal());
            }
            other => panic!("expected StatusUpdate, got {other:?}"),
        }
        assert!(!TaskState::Working.is_terminal());
        assert!(TaskState::Failed.is_terminal());
    }

    #[test]
    fn send_outcome_dispatches_task_vs_message() {
        let task = json!({
            "kind": "task",
            "id": "t1",
            "status": { "state": "completed", "message": {
                "kind": "message", "messageId": "m", "role": "agent",
                "parts": [{ "kind": "text", "text": "done" }]
            }}
        });
        assert_eq!(SendOutcome::from_value(task).unwrap().final_text(), "done");

        let bare_msg = json!({
            "kind": "message", "messageId": "m", "role": "agent",
            "parts": [{ "kind": "text", "text": "hi" }]
        });
        assert_eq!(
            SendOutcome::from_value(bare_msg).unwrap().final_text(),
            "hi"
        );
    }

    #[test]
    fn task_final_text_prefers_artifacts() {
        let task: Task = serde_json::from_value(json!({
            "id": "t",
            "status": { "state": "completed", "message": {
                "kind": "message", "messageId": "m", "role": "agent",
                "parts": [{ "kind": "text", "text": "status text" }]
            }},
            "artifacts": [{ "parts": [{ "kind": "text", "text": "artifact text" }] }]
        }))
        .unwrap();
        assert_eq!(task.final_text(), "artifact text");
    }

    #[test]
    fn agent_card_parses_and_resolves_endpoint() {
        // Minimal card with a top-level url.
        let card: AgentCard = serde_json::from_value(json!({
            "name": "flux",
            "url": "http://host/a2a",
            "capabilities": { "streaming": true }
        }))
        .unwrap();
        assert_eq!(card.name, "flux");
        assert!(card.capabilities.streaming);
        assert_eq!(card.rpc_endpoint().as_deref(), Some("http://host/a2a"));

        // Newer-style card with interfaces[] and no top-level url.
        let card2: AgentCard = serde_json::from_value(json!({
            "name": "other",
            "interfaces": [
                { "type": "GRPC", "url": "http://host/grpc" },
                { "transport": "JSONRPC", "url": "http://host/rpc" }
            ]
        }))
        .unwrap();
        assert_eq!(card2.rpc_endpoint().as_deref(), Some("http://host/rpc"));
    }

    #[test]
    fn client_url_normalization() {
        // Bare origin → <origin>/a2a.
        let c = A2aClient::new("http://127.0.0.1:8787").unwrap();
        assert_eq!(c.rpc_url(), "http://127.0.0.1:8787/a2a");
        // A pathful URL is used verbatim.
        let c2 = A2aClient::new("https://example.com/agents/foo").unwrap();
        assert_eq!(c2.rpc_url(), "https://example.com/agents/foo");
        // Garbage is rejected.
        assert!(A2aClient::new("not a url").is_err());
    }
}
