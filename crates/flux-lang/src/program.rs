//! The multi-agent **Program** layer: a `.flux` file may declare a whole app — agents, channels,
//! triggers, and journeys — not just a single flow. These are **pure-data declarations** (L0,
//! strings + an opaque `settings` JSON map); the L3 engine and the L6 `flux-app` host give them their
//! runtime *meaning* (model/datasource/channel wiring, the event bus, the scheduler). This keeps the
//! multi-agent vision coherent without expanding the language core: **orchestration is an op-pack**
//! (`ask`/`send`/`emit`/`spawn`), so this layer needs **zero new node kinds**.
//!
//! A bare single-flow file still loads — [`Module::from_json`] sniffs the shape and wraps a lone
//! [`DraftAst`]. "User input is just an event": a trigger's `on` label shares the event-label space
//! with [`crate::ast::Node::Await`].

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ast::DraftAst;
use crate::error::{FlowError, Result};

/// An agent: an identity plus the model / tool / datasource access surface it runs with. A superset
/// of an orchestration role; the L3 engine resolves the names to concrete capabilities.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
pub struct AgentDecl {
    pub name: String,
    /// The model this agent plans with (engine-resolved; `None` = the host default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The op/tool names this agent may call.
    #[serde(default)]
    pub tools: Vec<String>,
    /// The datasource names this agent may query.
    #[serde(default)]
    pub datasources: Vec<String>,
    /// A human-readable role description (seeds the agent's system framing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// An opaque settings bag — the L0 layer carries it verbatim; the engine interprets it.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub settings: serde_json::Value,
}

/// A channel: a named I/O surface the app listens and/or sends on (CLI, HTTP, Slack, …). The `kind`
/// selects the L6 channel runtime; `settings` configures it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
pub struct ChannelDecl {
    pub name: String,
    /// The channel runtime to use (e.g. `cli`, `http`, `slack`).
    pub kind: String,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub settings: serde_json::Value,
}

/// A trigger: an event→action binding. `on` is an event label (sharing the space with `Node::Await`);
/// when it fires the named `run` journey/flow executes, optionally as a specific `agent`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
pub struct TriggerDecl {
    pub name: String,
    /// The event label that fires this trigger (e.g. `user_input`, `cron:nightly`, a channel name).
    pub on: String,
    /// The journey (or flow) name to run when the trigger fires.
    pub run: String,
    /// The agent to run it as, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

/// A journey: a named flow embedding a [`DraftAst`], run by the existing interpreter unchanged.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
pub struct JourneyDecl {
    pub name: String,
    /// The agent that owns this journey, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// The flow body — an ordinary Draft AST.
    #[serde(default)]
    pub flow: DraftAst,
}

/// A whole multi-agent program (a module): agents, channels, triggers, journeys, and any top-level
/// flows. Every field defaults to empty, so a minimal program is valid.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, JsonSchema)]
pub struct Program {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default)]
    pub agents: Vec<AgentDecl>,
    #[serde(default)]
    pub channels: Vec<ChannelDecl>,
    #[serde(default)]
    pub triggers: Vec<TriggerDecl>,
    #[serde(default)]
    pub journeys: Vec<JourneyDecl>,
    /// Top-level flows (not owned by a journey) — e.g. an entrypoint a host may run directly.
    #[serde(default)]
    pub flows: Vec<DraftAst>,
}

impl Program {
    /// Resolve a journey or top-level flow by name to its `DraftAst`.
    pub fn flow_named(&self, name: &str) -> Option<&DraftAst> {
        self.journeys
            .iter()
            .find(|j| j.name == name)
            .map(|j| &j.flow)
            .or_else(|| self.flows.iter().find(|f| f.name.as_deref() == Some(name)))
    }
}

/// A loaded `.flux` module: either a single bare flow or a full multi-agent program. The loader
/// sniffs the shape so a host can accept both `foo.flux` (one flow) and `app.flux` (a program).
#[derive(Debug, Clone, PartialEq)]
pub enum Module {
    /// A bare single-flow file (a lone `DraftAst`).
    Flow(DraftAst),
    /// A multi-agent program.
    Program(Program),
}

impl Module {
    /// The keys that mark a document as a [`Program`] rather than a bare flow. A document carrying any
    /// of these is a program; otherwise it loads as a single flow.
    const PROGRAM_KEYS: [&'static str; 5] = ["agents", "channels", "triggers", "journeys", "flows"];

    /// Load a `.flux` JSON document, sniffing whether it is a bare [`DraftAst`] or a [`Program`].
    pub fn from_json(v: serde_json::Value) -> Result<Module> {
        let is_program = Self::PROGRAM_KEYS.iter().any(|k| v.get(k).is_some());
        if is_program {
            serde_json::from_value(v)
                .map(Module::Program)
                .map_err(|e| FlowError::Parse(format!("program: {e}")))
        } else {
            serde_json::from_value(v)
                .map(Module::Flow)
                .map_err(|e| FlowError::Parse(format!("flow: {e}")))
        }
    }

    /// Load a `.flux` module from a JSON string.
    pub fn parse_str(s: &str) -> Result<Module> {
        let v: serde_json::Value =
            serde_json::from_str(s).map_err(|e| FlowError::Parse(format!("json: {e}")))?;
        Self::from_json(v)
    }

    /// The [`Program`] if this module is one, else `None` (a bare flow is not implicitly a program).
    pub fn program(&self) -> Option<&Program> {
        match self {
            Module::Program(p) => Some(p),
            Module::Flow(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_flow_loads_as_a_single_flow() {
        let m = Module::parse_str(r#"{"name":"greet","body":[]}"#).unwrap();
        match m {
            Module::Flow(f) => assert_eq!(f.name.as_deref(), Some("greet")),
            Module::Program(_) => panic!("a bare flow must not sniff as a program"),
        }
    }

    #[test]
    fn a_document_with_agents_loads_as_a_program() {
        let src = r#"{
            "name": "support-app",
            "agents": [{"name": "triager", "model": "claude-opus-4-8", "tools": ["read", "grep"]}],
            "channels": [{"name": "cli", "kind": "cli"}],
            "triggers": [{"name": "t", "on": "user_input", "run": "handle"}],
            "journeys": [{"name": "handle", "flow": {"body": []}}]
        }"#;
        let Module::Program(p) = Module::parse_str(src).unwrap() else {
            panic!("a document with `agents` must sniff as a program");
        };
        assert_eq!(p.name.as_deref(), Some("support-app"));
        assert_eq!(p.agents.len(), 1);
        assert_eq!(p.agents[0].tools, vec!["read", "grep"]);
        assert_eq!(p.channels[0].kind, "cli");
        assert_eq!(p.triggers[0].on, "user_input");
        assert!(p.flow_named("handle").is_some(), "journey resolves by name");
    }

    #[test]
    fn program_round_trips_through_json() {
        let p = Program {
            name: Some("a".into()),
            agents: vec![AgentDecl {
                name: "x".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let s = serde_json::to_string(&p).unwrap();
        let back: Program = serde_json::from_str(&s).unwrap();
        assert_eq!(p, back);
    }
}
