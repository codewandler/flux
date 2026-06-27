//! The [`App`]: the runtime host that turns a parsed [`Program`] into a running multi-agent system.
//!
//! An app owns three things — the program (agents/channels/triggers/journeys), a [`ToolRegistry`]
//! assembled from the builtins + the orchestration op-pack (+ cognition ops when a provider is wired),
//! and the in-process [`Bus`]. The [`Engine`] behind it is the worker the public surface delegates to
//! (and that the `spawn` op re-enters); it is held in an `Arc` so the orchestration ops can hold a
//! `Weak` back-reference without a cycle.
//!
//! A journey is executed by **reusing flux-flow's engine path**: a real [`Executor`] (the full
//! permission + approval envelope) drives `flux_flow::runtime::execute_flow` over the journey's
//! `DraftAst`, with a [`FlowStore`] for state and an [`AgentSink`] for output. Nothing about the
//! interpreter is reinvented here — the multi-agent layer is pure wiring over the existing engine.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::broadcast::error::{RecvError, TryRecvError};

use flux_agent::AgentSink;
use flux_core::{Error, Result};
use flux_flow::state::FlowStore;
use flux_lang::ast::{SymbolName, Value as FluxValue, Visibility};
use flux_lang::program::Program;
use flux_provider::Provider;
use flux_runtime::{AllowApprover, Executor, PermissionManager, ToolContext, ToolRegistry};
use flux_system::{System, Workspace};

use crate::bus::{Bus, Event};
use crate::ops::{self, JourneyHost};

/// How deep `spawn`-within-`spawn` may recurse before the engine refuses (cheap guard against a
/// journey that spawns itself unboundedly).
const MAX_SPAWN_DEPTH: u32 = 16;

/// The most events a single [`App::deliver`] will process across the initial event and any it cascades
/// into (via `emit`). Bounds an `emit`-loop in the one-shot path; the long-running [`App::run`] loop is
/// unbounded by design.
const MAX_CASCADE: u32 = 256;

/// The result of running one journey: which journey, its textual result (the flow's `return`/last view),
/// and how many ops it dispatched.
#[derive(Debug, Clone)]
pub struct JourneyRun {
    pub journey: String,
    pub result: String,
    pub steps: usize,
}

/// The runtime host for a multi-agent [`Program`]. Cheap to clone is *not* a goal — hold one `App` and
/// drive it; clone the [`Bus`] handle (via [`App::bus`]) if another task needs to emit.
pub struct App {
    engine: Arc<Engine>,
}

impl App {
    /// Build a host for `program`. When `provider` is `Some`, the model-backed cognition ops
    /// (`ai.*`, `synth`) are registered too, so journeys may plan/extract/judge; with `None` the host
    /// is hermetic (pure ops only — no network, no model).
    pub fn new(
        program: Program,
        provider: Option<Arc<dyn Provider>>,
        model: impl Into<String>,
    ) -> Self {
        App {
            engine: Engine::new(program, provider, model.into()),
        }
    }

    /// The program this host runs.
    pub fn program(&self) -> &Program {
        &self.engine.program
    }

    /// The assembled op registry (builtins + orchestration + optional cognition).
    pub fn registry(&self) -> &ToolRegistry {
        &self.engine.registry
    }

    /// A handle to the event bus (clone it to emit from another task).
    pub fn bus(&self) -> &Bus {
        &self.engine.bus
    }

    /// Inject one event and run every journey its label triggers **to completion**, returning each
    /// run's result. Events the journeys `emit` are processed too (bounded by [`MAX_CASCADE`]). This is
    /// the unit of work tests and the CLI channels drive; [`App::run`] is the long-running form.
    pub async fn deliver(
        &self,
        label: impl Into<String>,
        payload: Value,
    ) -> Result<Vec<JourneyRun>> {
        self.engine.deliver(label.into(), payload).await
    }

    /// Run as a long-lived supervisor: emit a single `startup` event, then consume the bus forever,
    /// running each event's triggered journeys. Returns only when the bus closes. Cascaded `emit`s are
    /// picked up by the same loop, so this is the natural form for a server-style program.
    pub async fn run(&self) -> Result<()> {
        let mut rx = self.engine.bus.subscribe();
        // Fire startup *after* subscribing so a `{on:"startup"}` trigger is never missed.
        self.engine.bus.emit("startup", json!({}));
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let mut sink = RecordingSink::default();
                    self.engine
                        .run_triggers(&ev.label, &ev.payload, &mut sink)
                        .await?;
                }
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
        Ok(())
    }
}

/// The worker behind [`App`]. Owns the program, the registry, and the bus; resolves and runs journeys.
/// Held in an `Arc` so the `spawn` op can re-enter it through a `Weak<dyn JourneyHost>`.
pub(crate) struct Engine {
    pub(crate) program: Program,
    pub(crate) registry: ToolRegistry,
    pub(crate) bus: Bus,
    /// Active `spawn` recursion depth (guards against unbounded self-spawn).
    depth: AtomicU32,
    /// Monotonic counter giving each journey run a distinct session id.
    runs: AtomicU64,
}

impl Engine {
    fn new(program: Program, provider: Option<Arc<dyn Provider>>, model: String) -> Arc<Self> {
        let bus = Bus::new();
        let channels = Arc::new(program.channels.clone());
        // `new_cyclic`: the `spawn` op needs a back-reference to the engine it re-enters, but the
        // engine owns the registry that owns the op — a `Weak` breaks the cycle.
        Arc::new_cyclic(|weak: &Weak<Engine>| {
            let mut registry = ToolRegistry::new();
            flux_tools::register_builtins(&mut registry);
            if let Some(provider) = provider {
                flux_cognition::CognitionPack::new(provider, model).register(&mut registry);
            }
            let host: Weak<dyn JourneyHost> = weak.clone();
            ops::register(&mut registry, bus.clone(), channels, host);
            Engine {
                program,
                registry,
                bus,
                depth: AtomicU32::new(0),
                runs: AtomicU64::new(0),
            }
        })
    }

    /// Run every trigger whose `on` label equals `label`, collecting each journey run.
    async fn run_triggers(
        &self,
        label: &str,
        payload: &Value,
        sink: &mut dyn AgentSink,
    ) -> Result<Vec<JourneyRun>> {
        let mut runs = Vec::new();
        for trigger in self.program.triggers.iter().filter(|t| t.on == label) {
            runs.push(self.run_journey(&trigger.run, payload, sink).await?);
        }
        Ok(runs)
    }

    /// One-shot delivery: process the initial event, then drain and process any events its journeys
    /// emitted, repeating until the bus is quiet or [`MAX_CASCADE`] events have been handled.
    async fn deliver(&self, label: String, payload: Value) -> Result<Vec<JourneyRun>> {
        // Subscribe first so emits *during* the journeys are captured.
        let mut rx = self.bus.subscribe();
        let mut sink = RecordingSink::default();
        let mut pending: VecDeque<Event> = VecDeque::new();
        pending.push_back(Event::new(label, payload));
        let mut results = Vec::new();
        let mut budget = MAX_CASCADE;

        while let Some(ev) = pending.pop_front() {
            if budget == 0 {
                break;
            }
            budget -= 1;
            results.extend(self.run_triggers(&ev.label, &ev.payload, &mut sink).await?);
            // Drain events the journeys just emitted into the work queue.
            loop {
                match rx.try_recv() {
                    Ok(e) => pending.push_back(e),
                    Err(TryRecvError::Empty) | Err(TryRecvError::Closed) => break,
                    Err(TryRecvError::Lagged(_)) => continue,
                }
            }
        }
        Ok(results)
    }

    /// Execute one named journey to completion, reusing flux-flow's engine path (full envelope).
    async fn run_journey(
        &self,
        name: &str,
        payload: &Value,
        sink: &mut dyn AgentSink,
    ) -> Result<JourneyRun> {
        let ast = self
            .program
            .flow_named(name)
            .cloned()
            .ok_or_else(|| Error::Other(format!("unknown journey `{name}`")))?;

        // Depth guard: increment, ensure we decrement on every exit, then check.
        let prev = self.depth.fetch_add(1, Ordering::SeqCst);
        let _guard = DepthGuard(&self.depth);
        if prev >= MAX_SPAWN_DEPTH {
            return Err(Error::Other(format!(
                "spawn recursion exceeded max depth {MAX_SPAWN_DEPTH}"
            )));
        }

        let store = FlowStore::in_memory().map_err(other)?;
        let session_id = format!("{name}#{}", self.runs.fetch_add(1, Ordering::SeqCst));
        seed_payload(&store, &session_id, payload)?;
        let executor = build_executor(self.registry.clone())?;

        let outcome = flux_flow::runtime::execute_flow(&store, &executor, &session_id, &ast, sink)
            .await
            .map_err(other)?;

        Ok(JourneyRun {
            journey: name.to_string(),
            result: outcome.result,
            steps: outcome.steps,
        })
    }
}

#[async_trait]
impl JourneyHost for Engine {
    async fn run_journey_for_spawn(&self, name: &str, payload: Value) -> Result<String> {
        let mut sink = RecordingSink::default();
        Ok(self.run_journey(name, &payload, &mut sink).await?.result)
    }
}

/// Build the execution envelope for a journey: a guarded [`System`] rooted at the current working
/// directory, an [`AllowApprover`] (the host is non-interactive — it runs trusted, pre-authored
/// programs), and the shared op registry. Every op still dispatches through this `Executor`, so
/// permission rules and effect gating apply exactly as in the interactive engine.
fn build_executor(registry: ToolRegistry) -> Result<Executor> {
    let root = std::env::current_dir().map_err(other)?;
    let workspace = Workspace::new(&root).map_err(other)?;
    let system = Arc::new(System::new(workspace));
    let ctx = ToolContext::new(system);
    // Pre-allow the orchestration verbs + read-only builtins; anything else falls to the approver,
    // which auto-allows (a headless host). Tightening this is a deployment concern, not an MVP one.
    let allow: Vec<String> = [
        "emit", "send", "ask", "spawn", "read", "glob", "grep", "search",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    let perms = PermissionManager::from_rules(&allow, &[]);
    Ok(Executor::new(registry, perms, Arc::new(AllowApprover), ctx))
}

/// Seed an event's payload into the journey's session so the flow can read it: the whole payload binds
/// to `$input`, and each top-level field binds to its own symbol (so a journey body can interpolate
/// `{text}` or reference `$text` directly).
fn seed_payload(store: &FlowStore, session_id: &str, payload: &Value) -> Result<()> {
    bind_symbol(store, session_id, "input", payload)?;
    if let Some(obj) = payload.as_object() {
        for (key, value) in obj {
            bind_symbol(store, session_id, key, value)?;
        }
    }
    Ok(())
}

fn bind_symbol(store: &FlowStore, session_id: &str, name: &str, value: &Value) -> Result<()> {
    let flux_value = FluxValue::from_json(value);
    let value_id = store.put_value(session_id, &flux_value).map_err(other)?;
    store
        .bind(
            session_id,
            &SymbolName(name.to_string()),
            &value_id,
            None,
            &summarize(value),
            Visibility::Visible,
        )
        .map_err(other)?;
    Ok(())
}

/// A short human summary of a seeded value (the raw string for a string; compact JSON otherwise),
/// capped so a large payload doesn't bloat the session view.
fn summarize(value: &Value) -> String {
    let s = match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    if s.len() > 200 {
        format!("{}…", &s[..200])
    } else {
        s
    }
}

/// Map any foreign error onto [`flux_core::Error::Other`].
fn other(e: impl std::fmt::Display) -> Error {
    Error::Other(e.to_string())
}

/// Decrements the active spawn depth when a journey run unwinds (success, error, or early return).
struct DepthGuard<'a>(&'a AtomicU32);

impl Drop for DepthGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A minimal [`AgentSink`] that records streamed text and the op names dispatched. The journey's
/// canonical result is taken from the `FlowOutcome`, so this only needs to capture for inspection.
#[derive(Default)]
pub struct RecordingSink {
    pub text: String,
    pub tools: Vec<String>,
}

impl AgentSink for RecordingSink {
    fn text_delta(&mut self, text: &str) {
        self.text.push_str(text);
    }
    fn tool_call(&mut self, name: &str, _input: &Value) {
        self.tools.push(name.to_string());
    }
}
