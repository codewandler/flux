//! A2A protocol wire types (the HTTP + JSON-RPC 2.0 binding).
//!
//! These mirror the current A2A spec objects — `Message`/`Part`, `Task`/`TaskStatus`,
//! the streaming `TaskStatusUpdateEvent`/`TaskArtifactUpdateEvent`, and the `AgentCard` — and the
//! JSON-RPC envelope used by `message/send`, `message/stream`, and `tasks/get`. They are shared by
//! both the [`crate::A2aClient`] and `flux-server`'s A2A endpoint, so the wire has one definition.
//!
//! Types are deliberately permissive: unknown fields are ignored and most fields default, so cards
//! and messages from other A2A agents parse even when they carry extensions we don't model.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Mint an opaque random id (for `messageId` / JSON-RPC request ids). 128 random bits as hex.
pub fn new_id() -> String {
    let n: u128 = rand::random();
    format!("{n:032x}")
}

fn kind_message() -> String {
    "message".to_string()
}
fn kind_task() -> String {
    "task".to_string()
}
fn kind_status_update() -> String {
    "status-update".to_string()
}
fn kind_artifact_update() -> String {
    "artifact-update".to_string()
}

// ── Message / Part ──────────────────────────────────────────────────────────

/// Who authored a [`Message`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Agent,
}

/// A single content part. Modeled as a struct (not an enum) so any `kind` round-trips and unknown
/// part shapes (file/data + their fields) survive via [`Part::extra`]. The MVP only emits/reads text.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct Part {
    /// Discriminator: `"text"`, `"file"`, `"data"`, …
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Any non-text fields (file/data payloads, metadata) preserved verbatim.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Part {
    pub fn text(s: impl Into<String>) -> Self {
        Part {
            kind: "text".to_string(),
            text: Some(s.into()),
            extra: Map::new(),
        }
    }

    /// An A2A `data` part carrying an arbitrary structured payload under the spec's `data` field
    /// (e.g. a typed UI block a consumer's chat surface renders natively).
    pub fn data(value: Value) -> Self {
        let mut extra = Map::new();
        extra.insert("data".to_string(), value);
        Part {
            kind: "data".to_string(),
            text: None,
            extra,
        }
    }

    /// The text if this is a text part, else `None`.
    pub fn as_text(&self) -> Option<&str> {
        if self.kind == "text" {
            self.text.as_deref()
        } else {
            None
        }
    }
}

/// An A2A message: a turn of conversation carrying ordered [`Part`]s.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct Message {
    #[serde(default = "kind_message")]
    pub kind: String,
    pub message_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub role: Role,
    #[serde(default)]
    pub parts: Vec<Part>,
}

impl Message {
    /// A user message with a fresh `messageId`, optionally bound to a conversation `contextId`.
    pub fn user(parts: Vec<Part>, context_id: Option<String>) -> Self {
        Message {
            kind: kind_message(),
            message_id: new_id(),
            context_id,
            task_id: None,
            role: Role::User,
            parts,
        }
    }

    /// A single-text-part user message.
    pub fn user_text(text: impl Into<String>, context_id: Option<String>) -> Self {
        Self::user(vec![Part::text(text)], context_id)
    }

    /// A single-text-part agent (assistant) message.
    pub fn agent_text(text: impl Into<String>) -> Self {
        Message {
            kind: kind_message(),
            message_id: new_id(),
            context_id: None,
            task_id: None,
            role: Role::Agent,
            parts: vec![Part::text(text)],
        }
    }

    /// Concatenate all text parts.
    pub fn text(&self) -> String {
        self.parts
            .iter()
            .filter_map(Part::as_text)
            .collect::<Vec<_>>()
            .join("")
    }
}

// ── Task / status ───────────────────────────────────────────────────────────

/// Lifecycle state of a [`Task`]. JSON values are lowercase, kebab-cased.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum TaskState {
    Submitted,
    Working,
    InputRequired,
    Completed,
    Canceled,
    Failed,
    Rejected,
    AuthRequired,
    #[serde(other)]
    Unknown,
}

impl TaskState {
    /// True once the task has reached a final state (no more updates expected).
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TaskState::Completed | TaskState::Canceled | TaskState::Failed | TaskState::Rejected
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct TaskStatus {
    pub state: TaskState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

impl TaskStatus {
    pub fn new(state: TaskState, message: Option<Message>, timestamp: Option<String>) -> Self {
        TaskStatus {
            state,
            message,
            timestamp,
        }
    }
}

/// A produced output unit (a file, a block of text, …). We only read its text parts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub parts: Vec<Part>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A stateful unit of work the remote agent runs on our behalf.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct Task {
    #[serde(default = "kind_task")]
    pub kind: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    pub status: TaskStatus,
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
    #[serde(default)]
    pub history: Vec<Message>,
}

impl Task {
    /// A task carrying just a status (no artifacts/history) — what a server returns from a
    /// blocking `message/send`.
    pub fn new(id: impl Into<String>, context_id: Option<String>, status: TaskStatus) -> Self {
        Task {
            kind: kind_task(),
            id: id.into(),
            context_id,
            status,
            artifacts: Vec::new(),
            history: Vec::new(),
        }
    }

    /// The agent's reply text: artifacts first (spec agents often answer there), else the text of
    /// the message attached to the final status.
    pub fn final_text(&self) -> String {
        let mut out = String::new();
        for a in &self.artifacts {
            for p in &a.parts {
                if let Some(t) = p.as_text() {
                    out.push_str(t);
                }
            }
        }
        if out.is_empty() {
            if let Some(m) = &self.status.message {
                out = m.text();
            }
        }
        out
    }
}

// ── Streaming events ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct TaskStatusUpdateEvent {
    #[serde(default = "kind_status_update")]
    pub kind: String,
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    pub status: TaskStatus,
    /// `final` on the wire (a Rust keyword) — `true` on the last event of the stream.
    #[serde(default, rename = "final")]
    pub is_final: bool,
}

impl TaskStatusUpdateEvent {
    pub fn new(
        task_id: impl Into<String>,
        context_id: Option<String>,
        status: TaskStatus,
        is_final: bool,
    ) -> Self {
        TaskStatusUpdateEvent {
            kind: kind_status_update(),
            task_id: task_id.into(),
            context_id,
            status,
            is_final,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct TaskArtifactUpdateEvent {
    #[serde(default = "kind_artifact_update")]
    pub kind: String,
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    pub artifact: Artifact,
    #[serde(default)]
    pub append: bool,
    #[serde(default)]
    pub last_chunk: bool,
}

/// One decoded `result` payload from a `message/stream` SSE frame.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    Task(Task),
    Message(Message),
    StatusUpdate(TaskStatusUpdateEvent),
    ArtifactUpdate(TaskArtifactUpdateEvent),
}

impl StreamEvent {
    /// Dispatch a raw `result` value on its `kind` discriminator. Tolerant of agents that omit
    /// `kind`: a payload with a `status` is treated as a Task, one with `parts` as a Message.
    pub fn from_value(v: Value) -> Result<Self, serde_json::Error> {
        let kind = v.get("kind").and_then(Value::as_str).unwrap_or("");
        Ok(match kind {
            "task" => StreamEvent::Task(serde_json::from_value(v)?),
            "status-update" => StreamEvent::StatusUpdate(serde_json::from_value(v)?),
            "artifact-update" => StreamEvent::ArtifactUpdate(serde_json::from_value(v)?),
            "message" => StreamEvent::Message(serde_json::from_value(v)?),
            _ if v.get("status").is_some() => StreamEvent::Task(serde_json::from_value(v)?),
            _ => StreamEvent::Message(serde_json::from_value(v)?),
        })
    }
}

/// The result of `message/send`: the spec allows either a [`Task`] or a bare [`Message`].
#[derive(Debug, Clone)]
pub enum SendOutcome {
    Task(Task),
    Message(Message),
}

impl SendOutcome {
    pub fn from_value(v: Value) -> Result<Self, serde_json::Error> {
        let kind = v.get("kind").and_then(Value::as_str).unwrap_or("");
        Ok(match kind {
            "message" => SendOutcome::Message(serde_json::from_value(v)?),
            "task" => SendOutcome::Task(serde_json::from_value(v)?),
            _ if v.get("parts").is_some() && v.get("status").is_none() => {
                SendOutcome::Message(serde_json::from_value(v)?)
            }
            _ => SendOutcome::Task(serde_json::from_value(v)?),
        })
    }

    /// The underlying task, if this outcome is a task.
    pub fn as_task(&self) -> Option<&Task> {
        match self {
            SendOutcome::Task(t) => Some(t),
            SendOutcome::Message(_) => None,
        }
    }

    pub fn final_text(&self) -> String {
        match self {
            SendOutcome::Task(t) => t.final_text(),
            SendOutcome::Message(m) => m.text(),
        }
    }
}

// ── Agent card ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    #[serde(default)]
    pub streaming: bool,
    #[serde(default)]
    pub push_notifications: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct Skill {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_modes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_modes: Vec<String>,
}

/// A newer-spec transport interface declaration (`{ transport|type, url }`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct AgentInterface {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, alias = "type", skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
}

/// The A2A discovery document served at `/.well-known/agent-card.json` (or `…/agent.json`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct AgentCard {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// The JSON-RPC endpoint URL (preferred RPC target).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub capabilities: Capabilities,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub default_input_modes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub default_output_modes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<Skill>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interfaces: Vec<AgentInterface>,
    /// Declared authentication schemes, keyed by scheme name. The A2A spec (v1.0) requires
    /// clients to authenticate "using one of the schemes declared" in the card's
    /// `securitySchemes`/`security` fields — so an auth-enforcing server must populate these to
    /// be conformant. Values stay raw [`Value`]s because scheme objects are polymorphic
    /// (`http` / `apiKey` / `oauth2` / `openIdConnect` / `mutualTLS`). Absent → the key is
    /// omitted entirely, so pre-existing cards serialize byte-identically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security_schemes: Option<BTreeMap<String, Value>>,
    /// The spec's security requirement array: each entry maps a scheme name (a key of
    /// [`security_schemes`](Self::security_schemes)) to the scopes the caller must hold (empty =
    /// the scheme with no particular scopes). Absent → the key is omitted, as above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security: Option<Vec<BTreeMap<String, Vec<String>>>>,
}

impl AgentCard {
    /// The RPC endpoint to POST JSON-RPC to: the top-level `url`, else a JSON-RPC `interfaces[]`
    /// entry's url, else `None` (caller falls back to a derived `<base>/a2a`).
    pub fn rpc_endpoint(&self) -> Option<String> {
        if let Some(u) = &self.url {
            return Some(u.clone());
        }
        self.interfaces
            .iter()
            .find(|i| {
                i.transport
                    .as_deref()
                    .map(|t| t.to_ascii_lowercase().contains("jsonrpc"))
                    .unwrap_or(false)
            })
            .and_then(|i| i.url.clone())
            .or_else(|| self.interfaces.iter().find_map(|i| i.url.clone()))
    }

    /// Declare the card's authentication schemes (builder-style): scheme name → scheme object,
    /// serialized as the spec's `securitySchemes`. See
    /// [`security_schemes`](Self::security_schemes).
    pub fn with_security_schemes(mut self, schemes: BTreeMap<String, Value>) -> Self {
        self.security_schemes = Some(schemes);
        self
    }

    /// Declare the card's security requirements (builder-style): each entry maps a declared
    /// scheme name to the scopes the caller must hold. See [`security`](Self::security).
    pub fn with_security(mut self, security: Vec<BTreeMap<String, Vec<String>>>) -> Self {
        self.security = Some(security);
        self
    }
}

// ── JSON-RPC envelope ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct JsonRpcRequest<P> {
    pub jsonrpc: &'static str,
    pub id: String,
    pub method: String,
    pub params: P,
}

impl<P> JsonRpcRequest<P> {
    pub fn new(method: impl Into<String>, params: P) -> Self {
        JsonRpcRequest {
            jsonrpc: "2.0",
            id: new_id(),
            method: method.into(),
            params,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
// `#[serde(default)]` on a generic field would otherwise make serde infer a spurious `T: Default`
// bound; pin the deserialize bound to just `Deserialize`.
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
pub struct JsonRpcResponse<T> {
    #[serde(default)]
    pub result: Option<T>,
    #[serde(default)]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(default)]
    pub data: Option<Value>,
}

/// `params` for `message/send` and `message/stream`.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct SendMessageParams {
    pub message: Message,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configuration: Option<SendConfiguration>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct SendConfiguration {
    pub blocking: bool,
}

/// `params` for `tasks/get`.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct TaskGetParams {
    pub id: String,
}

// ── Schema derives (D-57) ────────────────────────────────────────────────────
//
// Proves the `utoipa` feature actually compiles a faithful schema for the wire types a consumer
// documents (`ai-agents/src/openapi.rs`'s hand-mirrored structs), not just that the derive macro
// runs without complaint: it inspects the generated `Object` schema shape (property names honor
// `#[serde(rename_all)]`, enums carry their `#[serde(rename_all)]`-cased variants) for the two
// richest root types — [`AgentCard`] (the discovery document) and [`Message`] (nested through
// `TaskStatus`/`Task`) — plus their enum fields ([`Role`], [`TaskState`]).

#[cfg(all(test, feature = "utoipa"))]
mod schema_tests {
    use super::*;
    use utoipa::openapi::{RefOr, Schema};
    use utoipa::PartialSchema;

    /// Unwrap a top-level `object` schema (not a `$ref` or a composite `allOf`/`oneOf`).
    fn as_object(schema: RefOr<Schema>) -> utoipa::openapi::schema::Object {
        match schema {
            RefOr::T(Schema::Object(obj)) => obj,
            RefOr::Ref(_) => panic!("expected an inline object schema, got a $ref"),
            RefOr::T(_) => panic!("expected an inline object schema, got a composite schema"),
        }
    }

    #[test]
    fn agent_card_schema_exposes_the_discovery_document_fields() {
        let obj = as_object(AgentCard::schema());
        for field in [
            "name",
            "description",
            "url",
            "version",
            "capabilities",
            "defaultInputModes",
            "defaultOutputModes",
            "skills",
            "interfaces",
            "securitySchemes",
            "security",
        ] {
            assert!(
                obj.properties.contains_key(field),
                "AgentCard schema missing `{field}`: {:?}",
                obj.properties.keys().collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn message_schema_uses_camel_case_field_names() {
        let obj = as_object(Message::schema());
        for field in ["kind", "messageId", "contextId", "taskId", "role", "parts"] {
            assert!(
                obj.properties.contains_key(field),
                "Message schema missing `{field}`: {:?}",
                obj.properties.keys().collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn role_schema_is_a_lowercase_two_value_enum() {
        let obj = as_object(Role::schema());
        let values: Vec<String> = obj
            .enum_values
            .expect("Role is a fieldless enum")
            .iter()
            .map(|v| v.as_str().expect("string enum value").to_string())
            .collect();
        assert_eq!(values.len(), 2, "unexpected Role variants: {values:?}");
        assert!(values.contains(&"user".to_string()));
        assert!(values.contains(&"agent".to_string()));
    }

    #[test]
    fn task_state_schema_is_kebab_case_and_carries_the_unknown_fallback() {
        let obj = as_object(TaskState::schema());
        let values: Vec<String> = obj
            .enum_values
            .expect("TaskState is a fieldless enum")
            .iter()
            .map(|v| v.as_str().expect("string enum value").to_string())
            .collect();
        assert!(values.contains(&"completed".to_string()));
        assert!(values.contains(&"input-required".to_string()));
        // `#[serde(other)]`'s fallback variant renders as its own value — not perfectly faithful
        // to serde's catch-all semantics, but JSON Schema has no equivalent, and the value list
        // still enumerates every state a client can observe.
        assert!(values.contains(&"unknown".to_string()));
    }
}
