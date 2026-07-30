//! flux-flow's own durable store: the immutable value store, the session symbol table, and the
//! suspended-flow latch — the *mutable*, non-log-shaped execution state.
//!
//! Run-event traces no longer live here: they are appended to the unified [`EventStore`] (one ordered
//! log shared with the conversation and turn telemetry), which `FlowStore` forwards to through its
//! [`append_event`](FlowStore::append_event) impl and reads back via [`events`](FlowStore::events).
//! What stays is genuinely not log-shaped: values (content-addressed blobs), symbols (a
//! last-writer-wins pointer table), and the one-shot suspension latch.
//!
//! Values are append-only and versioned: a revision creates a new [`ValueId`] and the old version
//! stays addressable. A symbol points at its *current* value; the symbol table is the model-facing
//! projection mechanism, and only visible/pinned symbols appear in [`FlowStore::view`].
//!
//! **Storage is a port** (C-270). [`FlowStore`] holds an `Arc<dyn FlowStateBackend>` and owns
//! everything above it: serialization, the projection policy, the timestamps, and the run-event
//! forwarding. `SqliteState` is the native backend and the default for every existing constructor,
//! so nothing about native behaviour changed; [`MemoryState`] is a driver-free implementation of the
//! same port, and [`FlowStore::with_backend`] is how an embedder supplies its own. The engine
//! therefore no longer reaches a database directly — see [`port`] for why "no such row" had to become
//! the port's own outcome rather than a driver error.
//!
//! C-274 finished the job the port left open: the driver itself is now a **feature** (`sqlite`, on by
//! default), so `--no-default-features` builds this crate with [`MemoryState`] as the only backend
//! and no C library anywhere in the graph. C-270 deliberately did not do this alone, because
//! `rusqlite` also reached here through `flux-events` (`Arc<EventStore>` is in [`FlowStore`]'s public
//! signature) — dropping one of two paths bought nothing. Both are gated now.

mod memory;
pub mod port;
#[cfg(feature = "sqlite")]
mod sqlite;

// Only the SQLite-backed constructor takes a filesystem path — see `FlowStore::open`.
#[cfg(feature = "sqlite")]
use std::path::Path;
use std::sync::{Arc, Mutex};

use flux_core::{Error, Result};
use flux_events::EventStore;

use crate::ast::{Node, NodeId, RunEvent, SymbolName, Value, ValueId, Visibility};

pub use memory::MemoryState;
pub use port::{FlowStateBackend, Lookup, StoredSymbol, Suspension, SymbolBinding};
#[cfg(feature = "sqlite")]
pub use sqlite::SqliteState;

/// Wall-clock milliseconds, read once per write and handed to the backend. The single clock
/// dependency left on this path — a backend never reads one itself, which is what lets a non-native
/// one exist at all (`SystemTime::now` is unavailable on `wasm32-unknown-unknown`).
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The model-facing session-projection types live in the language crate ([`flux_lang::store`]);
/// re-exported so `flux_flow::state::{SessionView, SymbolView}` paths are unchanged.
pub use flux_lang::store::{SessionView, SymbolView};

/// flux-flow's durable [`FlowStore`] is the engine's [`ValueStore`](flux_lang::store::ValueStore):
/// the interpreter (in `flux-lang`) reads and writes session state through this trait, with the
/// storage backend staying here. Methods forward to the inherent ones (inherent methods win in
/// `self.method()` resolution, so there is no recursion).
impl flux_lang::store::ValueStore for FlowStore {
    fn put_value(&self, session_id: &str, value: &Value) -> Result<ValueId> {
        self.put_value(session_id, value)
    }
    fn get_value(&self, id: &ValueId) -> Result<Option<Value>> {
        self.get_value(id)
    }
    fn bind(
        &self,
        session_id: &str,
        name: &SymbolName,
        vid: &ValueId,
        ty: Option<&str>,
        summary: &str,
        visibility: Visibility,
    ) -> Result<()> {
        self.bind(session_id, name, vid, ty, summary, visibility)
    }
    fn resolve(&self, session_id: &str, name: &SymbolName) -> Result<Option<ValueId>> {
        self.resolve(session_id, name)
    }
    fn append_event(&self, session_id: &str, event: &RunEvent) -> Result<()> {
        self.append_event(session_id, event)
    }
    fn view(&self, session_id: &str) -> Result<SessionView> {
        self.view(session_id)
    }
    fn as_durable(&self) -> Option<&dyn flux_lang::store::DurableStore> {
        Some(self)
    }
}

/// The engine's durable backend for the `once` at-most-once primitive: completions are folded out of
/// the append-only run-event log (the same log `await`/run-trace use), so history is never rewritten.
impl flux_lang::store::DurableStore for FlowStore {
    fn once_lookup(
        &self,
        session_id: &str,
        label: &str,
    ) -> Result<Option<flux_lang::store::OnceRecord>> {
        Ok(self
            .events(session_id)?
            .into_iter()
            .rev()
            .find_map(|e| match e {
                RunEvent::OnceCompleted {
                    label: l,
                    value,
                    summary,
                } if l == label => Some(flux_lang::store::OnceRecord { value, summary }),
                _ => None,
            }))
    }

    fn once_complete(
        &self,
        session_id: &str,
        label: &str,
        value: Option<&ValueId>,
        summary: &str,
    ) -> Result<()> {
        self.append_event(
            session_id,
            &RunEvent::OnceCompleted {
                label: label.to_string(),
                value: value.cloned(),
                summary: summary.to_string(),
            },
        )
    }

    fn checkpoint_resume(&self, session_id: &str, flow_key: &str) -> Result<Option<NodeId>> {
        Ok(self
            .events(session_id)?
            .into_iter()
            .rev()
            .find_map(|e| match e {
                RunEvent::CheckpointReached {
                    flow_key: f, node, ..
                } if f == flow_key => Some(node),
                _ => None,
            }))
    }

    fn checkpoint_record(
        &self,
        session_id: &str,
        flow_key: &str,
        label: &str,
        index: NodeId,
    ) -> Result<()> {
        self.append_event(
            session_id,
            &RunEvent::CheckpointReached {
                flow_key: flow_key.to_string(),
                label: label.to_string(),
                node: index,
            },
        )
    }
}

/// The open halt latch [`FlowStore::open_halted_plan`] returns: the unresumed authored-flow halt
/// alongside the ledger a corrected resume fast-forwards against.
#[derive(Debug, Clone)]
pub struct OpenHalt {
    /// The failed statement's identity, classification, and message.
    pub halt: flux_lang::runtime::PlanHalt,
    /// The completed-statement memory to resume against.
    pub ledger: flux_lang::runtime::ResumeLedger,
}

/// flux-flow's store for values, symbols, the suspension latch, and session composites. Storage is
/// reached through [`FlowStateBackend`] ([`SqliteState`] natively); run-event traces are forwarded to
/// the shared [`EventStore`] rather than stored here.
pub struct FlowStore {
    backend: Arc<dyn FlowStateBackend>,
    /// The unified event log this store forwards run-trace events to (and reads them back from).
    events: Arc<EventStore>,
    /// C-43: the active cassette scope (record / replay), if any. Rides on the store — the one
    /// handle every execution path already threads — so each `ExecutorHost` construction
    /// self-wires without signature churn (the A-20 `reads` precedent). `None` = cassette off.
    cassette: Mutex<Option<Arc<crate::cassette::CassetteScope>>>,
}

impl FlowStore {
    /// Open (creating if needed) a SQLite store at `path`, with WAL enabled. Run-trace events are
    /// forwarded to the shared `events` log.
    ///
    /// Needs the `sqlite` feature (on by default): this constructor promises durable state at a path,
    /// which the driver-free backend cannot deliver — quietly substituting a store that forgets
    /// everything would be worse than not compiling (C-274).
    #[cfg(feature = "sqlite")]
    pub fn open(path: impl AsRef<Path>, events: Arc<EventStore>) -> Result<Self> {
        Ok(Self::with_backend(
            Arc::new(SqliteState::open(path)?),
            events,
        ))
    }

    /// An in-memory store (for tests), with its own throwaway event log — in-memory **SQLite** with
    /// the `sqlite` feature on (the default, and unchanged behaviour), [`MemoryState`] when it is off.
    /// Present in every feature combination on purpose: this is how most of the engine's tests and
    /// several consumers build a throwaway store, and none of them cares which implementation forgets
    /// the data.
    pub fn in_memory() -> Result<Self> {
        Self::in_memory_with_events(Arc::new(EventStore::in_memory()?))
    }

    /// [`in_memory`](Self::in_memory) sharing a given event log — so the engine's run trace, message
    /// log, and turn telemetry all land in one place even in tests.
    pub fn in_memory_with_events(events: Arc<EventStore>) -> Result<Self> {
        #[cfg(feature = "sqlite")]
        let backend: Arc<dyn FlowStateBackend> = Arc::new(SqliteState::in_memory()?);
        #[cfg(not(feature = "sqlite"))]
        let backend: Arc<dyn FlowStateBackend> = Arc::new(MemoryState::default());
        Ok(Self::with_backend(backend, events))
    }

    /// Build a store over an arbitrary [`FlowStateBackend`] — the seam a non-native substrate uses
    /// (C-270). Every other constructor is this one with [`SqliteState`] supplied.
    pub fn with_backend(backend: Arc<dyn FlowStateBackend>, events: Arc<EventStore>) -> Self {
        Self {
            backend,
            events,
            cassette: Mutex::new(None),
        }
    }

    /// C-43: install (or clear) the active cassette scope. The engine arms a fresh `Record` scope
    /// per turn; the replay driver installs `Replay`; the fork engine swaps `Replay` → `Record` at
    /// the divergence point so the forked tail is itself replayable.
    pub fn set_cassette(&self, scope: Option<Arc<crate::cassette::CassetteScope>>) {
        *self.cassette.lock().unwrap() = scope;
    }

    /// The active cassette scope, if any (read by every `ExecutorHost` construction).
    pub fn cassette(&self) -> Option<Arc<crate::cassette::CassetteScope>> {
        self.cassette.lock().unwrap().clone()
    }

    /// The unified event log this store forwards to (C-43: the recorder appends cells here; the
    /// replay/fork drivers read traces back from it).
    pub fn event_store(&self) -> Arc<EventStore> {
        self.events.clone()
    }

    /// Store an immutable value and return its id. Values are append-only — a revision creates a new
    /// id; old versions remain addressable for audit and re-run.
    pub fn put_value(&self, session_id: &str, value: &Value) -> Result<ValueId> {
        let data = serde_json::to_string(value)?;
        let bytes = data.len() as i64;
        self.backend.put_value(session_id, &data, bytes, now_ms())
    }

    /// Fetch a stored value by id.
    pub fn get_value(&self, id: &ValueId) -> Result<Option<Value>> {
        match self.backend.get_value(id)?.found() {
            Some(data) => Ok(Some(serde_json::from_str(&data)?)),
            None => Ok(None),
        }
    }

    /// Bind a symbol to a value (creating it or moving the pointer). The previous value stays stored.
    pub fn bind(
        &self,
        session_id: &str,
        name: &SymbolName,
        value_id: &ValueId,
        ty: Option<&str>,
        summary: &str,
        visibility: Visibility,
    ) -> Result<()> {
        self.backend.bind(
            session_id,
            &name.0,
            SymbolBinding {
                value_id: &value_id.0,
                ty,
                summary,
                visibility,
            },
            now_ms(),
        )
    }

    /// Resolve a symbol to its current value id.
    pub fn resolve(&self, session_id: &str, name: &SymbolName) -> Result<Option<ValueId>> {
        Ok(self
            .backend
            .resolve(session_id, &name.0)?
            .found()
            .map(ValueId))
    }

    /// Pre-bind a named input so a flow's `$name` resolves to `value` **before** the run — the
    /// per-invocation value-injection seam a behaviour runner needs (run a stored flow with these
    /// settings, without baking them into the AST as `lit` nodes). `value` is natural JSON,
    /// canonicalized via [`flux_lang::runtime::lit_value`] to the same JSON-as-string shape the
    /// interpreter binds for a flow-body `lit` node — so a seeded `$name` is indistinguishable
    /// from a literal-bound one everywhere downstream, arg marshaling included (D-67: a lone
    /// seeded object string-wraps under an op's sole required param exactly like a literal;
    /// stored structurally it would pass through as the op's whole input instead).
    ///
    /// Bound [`Visibility::Hidden`]: the interpreter's [`resolve`](Self::resolve) sees it (so `$name`
    /// works), but it stays out of the model-facing [`view`](Self::view). A flow-local `bind` to the
    /// same name later overwrites the pointer (last-writer-wins), so the flow can shadow its inputs.
    pub fn seed(
        &self,
        session_id: &str,
        name: &SymbolName,
        value: &serde_json::Value,
    ) -> Result<()> {
        let vid = self.put_value(session_id, &flux_lang::runtime::lit_value(value))?;
        self.bind(
            session_id,
            name,
            &vid,
            None,
            "<seeded input>",
            Visibility::Hidden,
        )
    }

    /// Append a run-event to the session's trace (forwarded to the unified event log).
    pub fn append_event(&self, session_id: &str, event: &RunEvent) -> Result<()> {
        self.events.record_run_event(session_id, event)
    }

    /// Load the run-event trace for a session (projected from the unified event log).
    pub fn events(&self, session_id: &str) -> Result<Vec<RunEvent>> {
        self.events.run_trace(session_id)
    }

    /// The session's open **halt latch** (design `multipass-agent-loop.md` Part 2): the last
    /// unresumed [`RunEvent::PlanHalted`] plus the [`flux_lang::runtime::ResumeLedger`]
    /// `execute_flow_resumable` fast-forwards against. `None` when the session never halted, the
    /// halt was already consumed by a later `PlanResumed`, or a crash left a `StatementCompleted`
    /// trail with no closing `PlanHalted` (conservative — the next run starts fresh rather than
    /// guessing at a latch that was never durably recorded).
    ///
    /// Folded fresh from the run-event log on every call — the `once_lookup`/`checkpoint_resume`
    /// house pattern above, delegating the ledger half to [`flux_lang::runtime::ResumeLedger::fold`]
    /// so the one fold algorithm isn't duplicated. No new state table, so this is crash-tolerant and
    /// cross-process by construction: any `FlowStore` opened over the same `events.db` sees the same
    /// latch.
    pub fn open_halted_plan(&self, session_id: &str) -> Result<Option<OpenHalt>> {
        let events = self.events(session_id)?;
        let Some(ledger) = flux_lang::runtime::ResumeLedger::fold(&events) else {
            return Ok(None);
        };
        // The halt event itself (kind/node/stmt/op/message) — `ResumeLedger::fold` already proves
        // `ledger.prior_plan` is the one open (unresumed) plan key, so the matching `PlanHalted` is
        // the halt latch's own record. Reconstructed here (rather than threaded out of `fold`) to
        // keep that fold's return type focused on the ledger it was designed for.
        let halt = events.iter().rev().find_map(|e| match e {
            RunEvent::PlanHalted {
                plan,
                node,
                stmt,
                op,
                kind,
                error,
            } if *plan == ledger.prior_plan => Some(flux_lang::runtime::PlanHalt {
                node: *node,
                stmt: stmt.clone(),
                op: op.clone(),
                kind: *kind,
                message: error.clone(),
                plan: plan.clone(),
            }),
            _ => None,
        });
        Ok(halt.map(|halt| OpenHalt { halt, ledger }))
    }

    /// The persisted conversation for a session — the `user → assistant` message log projected from
    /// the unified event store. Model stages use it as their durable conversational history.
    pub fn conversation(&self, session_id: &str) -> Result<Vec<flux_core::Message>> {
        self.events.conversation(session_id)
    }

    /// Incremental conversation replay: the message/compacted events with `stream_seq > after_seq`,
    /// so a caller maintaining a cached conversation can fetch only what was appended since instead of
    /// re-reading the whole log every model stage. See [`EventStore::conversation_delta`].
    pub fn conversation_delta(
        &self,
        session_id: &str,
        after_seq: i64,
    ) -> Result<Vec<flux_events::StoredEvent>> {
        self.events.conversation_delta(session_id, after_seq)
    }

    /// Persist a session-scoped composite op definition as normalized Flux-Lang source.
    pub fn save_session_composite(&self, session_id: &str, name: &str, source: &str) -> Result<()> {
        self.backend
            .save_session_composite(session_id, name, source, now_ms())
    }

    /// Load every composite op definition registered for `session_id`.
    pub fn session_composites(&self, session_id: &str) -> Result<Vec<(String, String)>> {
        self.backend.session_composites(session_id)
    }

    /// Persist a flow suspended on a top-level `await`: the flow's declared name (if any), its body,
    /// the suspended node index, and the awaited input `source`. One pending suspension per session —
    /// a new one replaces any prior. The engine loads this recoverably and clears it only after the
    /// awaited continuation reaches a durable terminal outcome; lower-level callers may still use
    /// [`take_suspension`](Self::take_suspension) for explicit one-shot semantics. The name is part
    /// of the resume point on purpose: a *named* flow's
    /// checkpoint `flow_key` is name + body hash, so the resume must carry the name to record/read
    /// checkpoints under the same key the original run used (L-21).
    pub fn save_suspension(
        &self,
        session_id: &str,
        flow_name: Option<&str>,
        body: &[Node],
        node: NodeId,
        source: &str,
    ) -> Result<()> {
        let suspension = Suspension {
            flow_name: flow_name.map(str::to_string),
            body: serde_json::to_string(body)?,
            node: i64::from(node.0),
            source: source.to_string(),
        };
        self.backend
            .save_suspension(session_id, &suspension, now_ms())
    }

    /// Whether a session has a pending suspension — a **non-consuming** peek (unlike
    /// [`take_suspension`](Self::take_suspension), which loads and deletes). Used by a flow-driven
    /// voice session (D-132) to tell a re-suspended flow (keep speaking prompts) from a completed one
    /// (speak the final line, then hang up), after a turn has run.
    pub fn has_suspension(&self, session_id: &str) -> Result<bool> {
        self.backend.has_suspension(session_id)
    }

    /// Load a session's pending continuation without consuming it.
    ///
    /// Engine-driven resume uses this recoverable read and clears the row only after the resumed
    /// suffix completes (or atomically replaces it when the flow suspends again). Cancellation or
    /// failure therefore leaves the sole continuation checkpoint available for an explicit retry.
    #[allow(clippy::type_complexity)]
    pub fn load_suspension(
        &self,
        session_id: &str,
    ) -> Result<Option<(Option<String>, Vec<Node>, NodeId, String)>> {
        let Some(row) = self.backend.load_suspension(session_id)?.found() else {
            return Ok(None);
        };
        // Non-consuming, so an undeserializable body is reported rather than silently swallowed —
        // the latch is still there to retry against. (`take_suspension` recovers instead, because it
        // has already consumed it.)
        let body = serde_json::from_str::<Vec<Node>>(&row.body).map_err(|error| {
            Error::Other(format!(
                "stored suspension for session `{session_id}` is invalid: {error}"
            ))
        })?;
        Ok(Some((
            row.flow_name,
            body,
            NodeId(row.node as u32),
            row.source,
        )))
    }

    /// Clear a pending continuation after the engine has durably handled its terminal outcome.
    pub fn clear_suspension(&self, session_id: &str) -> Result<()> {
        self.backend.clear_suspension(session_id)
    }

    /// Take (load **and** remove) a session's pending suspension, if any — a one-shot resume point.
    /// Returns the persisted flow name (if the suspended flow was named), body, the suspended
    /// `await` node, and the awaited source.
    #[allow(clippy::type_complexity)]
    pub fn take_suspension(
        &self,
        session_id: &str,
    ) -> Result<Option<(Option<String>, Vec<Node>, NodeId, String)>> {
        let Some(row) = self.backend.take_suspension(session_id)?.found() else {
            return Ok(None);
        };
        // One-shot: the backend has already cleared the latch. A body that no longer deserializes
        // (e.g. AST schema drift across an upgrade) is discarded and reported as "no suspension" so
        // the turn recovers through a fresh adaptive drive rather than hard-erroring forever.
        match serde_json::from_str::<Vec<Node>>(&row.body) {
            Ok(body) => Ok(Some((
                row.flow_name,
                body,
                NodeId(row.node as u32),
                row.source,
            ))),
            Err(_) => Ok(None),
        }
    }

    /// Total stored value bytes for a session (the budget-accounting surface; eviction lands later).
    pub fn total_value_bytes(&self, session_id: &str) -> Result<u64> {
        self.backend.total_value_bytes(session_id)
    }

    /// Project the model-facing view: visible + pinned symbols, newest-updated first, summaries only.
    pub fn view(&self, session_id: &str) -> Result<SessionView> {
        let symbols = self
            .backend
            .symbols(session_id)?
            .into_iter()
            .filter(|s| s.visibility.is_shown())
            .map(|s| SymbolView {
                name: s.name,
                ty: s.ty,
                summary: s.summary,
                visibility: s.visibility,
            })
            .collect();
        Ok(SessionView { symbols })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suspensions_round_trip_take_once_and_replace() {
        let s = FlowStore::in_memory().unwrap();
        let body = vec![Node::Await {
            binding: Some(SymbolName("x".into())),
            source: "user_input".into(),
            as_type: None,
            condition: None,
        }];
        assert!(
            s.take_suspension("sess").unwrap().is_none(),
            "none initially"
        );

        s.save_suspension("sess", None, &body, NodeId(3), "user_input")
            .unwrap();
        // A second save replaces the first (one pending suspension per session) — and the flow's
        // declared name round-trips with the resume point (L-21: the resume needs it to derive the
        // same checkpoint `flow_key` the run used).
        s.save_suspension("sess", Some("wf"), &body, NodeId(5), "other")
            .unwrap();

        let (flow_name, got_body, node, source) =
            s.take_suspension("sess").unwrap().expect("a suspension");
        assert_eq!(node, NodeId(5), "latest save wins");
        assert_eq!(source, "other");
        assert_eq!(got_body, body);
        assert_eq!(
            flow_name.as_deref(),
            Some("wf"),
            "the flow name is part of the resume point"
        );
        // Taking is one-shot — it's cleared.
        assert!(s.take_suspension("sess").unwrap().is_none(), "consumed");
    }

    #[test]
    fn values_are_versioned_and_old_versions_stay_addressable() {
        let s = FlowStore::in_memory().unwrap();
        let v1 = s
            .put_value("sess", &Value::String("draft one".into()))
            .unwrap();
        let v2 = s
            .put_value("sess", &Value::String("draft two".into()))
            .unwrap();
        assert!(v1.0.starts_with("v_"));
        assert_ne!(v1, v2);
        assert_eq!(
            s.get_value(&v1).unwrap(),
            Some(Value::String("draft one".into()))
        );
        assert_eq!(
            s.get_value(&v2).unwrap(),
            Some(Value::String("draft two".into()))
        );
        assert!(s.get_value(&ValueId("v_9999".into())).unwrap().is_none());
    }

    #[test]
    fn bind_moves_the_pointer_but_keeps_the_old_value() {
        let s = FlowStore::in_memory().unwrap();
        let draft = SymbolName("draft".into());
        let v1 = s.put_value("sess", &Value::String("v1".into())).unwrap();
        let v2 = s.put_value("sess", &Value::String("v2".into())).unwrap();

        s.bind(
            "sess",
            &draft,
            &v1,
            Some("Draft"),
            "first",
            Visibility::Visible,
        )
        .unwrap();
        assert_eq!(s.resolve("sess", &draft).unwrap(), Some(v1.clone()));

        s.bind(
            "sess",
            &draft,
            &v2,
            Some("Draft"),
            "second",
            Visibility::Visible,
        )
        .unwrap();
        assert_eq!(s.resolve("sess", &draft).unwrap(), Some(v2));
        // the superseded value is still retrievable
        assert_eq!(s.get_value(&v1).unwrap(), Some(Value::String("v1".into())));
    }

    #[test]
    fn seed_then_resolve_round_trips() {
        let s = FlowStore::in_memory().unwrap();
        // A scalar seed resolves for `$greeting`.
        let greeting = SymbolName("greeting".into());
        s.seed("sess", &greeting, &serde_json::json!("hej"))
            .unwrap();
        let vid = s
            .resolve("sess", &greeting)
            .unwrap()
            .expect("a seeded var resolves");
        assert_eq!(
            s.get_value(&vid).unwrap(),
            Some(Value::String("hej".into()))
        );
        // A structured seed stores the interpreter's literal canonicalization (D-67): the compact
        // JSON *text* as a `Value::String` — the exact shape a flow-body `lit` bind produces — not
        // the structural value. (Bonus over the old structural form: no f64 round-trip, so the
        // integer `3` survives as `3`, not `3.0`.)
        let cfg = SymbolName("cfg".into());
        s.seed("sess", &cfg, &serde_json::json!({"n": 3})).unwrap();
        let vid = s.resolve("sess", &cfg).unwrap().unwrap();
        assert_eq!(
            s.get_value(&vid).unwrap(),
            Some(Value::String("{\"n\":3}".into()))
        );
    }

    #[test]
    fn seed_is_hidden_from_view() {
        let s = FlowStore::in_memory().unwrap();
        s.seed(
            "sess",
            &SymbolName("secret_input".into()),
            &serde_json::json!("x"),
        )
        .unwrap();
        let names: Vec<String> = s
            .view("sess")
            .unwrap()
            .symbols
            .iter()
            .map(|s| s.name.0.clone())
            .collect();
        assert!(
            !names.contains(&"secret_input".to_string()),
            "a seeded input must not appear in the model-facing view"
        );
    }

    #[test]
    fn a_bind_shadows_a_seed() {
        let s = FlowStore::in_memory().unwrap();
        let name = SymbolName("x".into());
        s.seed("sess", &name, &serde_json::json!("seeded")).unwrap();
        // A flow-local bind to the same name overwrites the pointer (last-writer-wins).
        let v = s.put_value("sess", &Value::String("bound".into())).unwrap();
        s.bind("sess", &name, &v, None, "flow-local", Visibility::Visible)
            .unwrap();
        let vid = s.resolve("sess", &name).unwrap().unwrap();
        assert_eq!(
            s.get_value(&vid).unwrap(),
            Some(Value::String("bound".into())),
            "a flow-local bind shadows the seed"
        );
    }

    #[test]
    fn flowstore_once_records_and_reads_back_durably() {
        use flux_lang::store::DurableStore;
        let s = FlowStore::in_memory().unwrap();
        // No record yet → the body would run.
        assert!(s.once_lookup("sess", "welcome").unwrap().is_none());
        // Record a completion (with a bound value), then it reads back from the event log.
        let v = s.put_value("sess", &Value::String("ok".into())).unwrap();
        s.once_complete("sess", "welcome", Some(&v), "sent")
            .unwrap();
        let rec = s
            .once_lookup("sess", "welcome")
            .unwrap()
            .expect("completion recorded");
        assert_eq!(rec.summary, "sent");
        assert_eq!(rec.value, Some(v));
        // A different label is independent; a different session is too.
        assert!(s.once_lookup("sess", "other").unwrap().is_none());
        assert!(s.once_lookup("other", "welcome").unwrap().is_none());
    }

    #[test]
    fn flowstore_checkpoint_records_and_resumes_durably() {
        use flux_lang::store::DurableStore;
        let s = FlowStore::in_memory().unwrap();
        // No checkpoint yet for this flow.
        assert!(s.checkpoint_resume("sess", "phased").unwrap().is_none());
        // Record reaching the checkpoint at top-level index 1.
        s.checkpoint_record("sess", "phased", "p1", NodeId(1))
            .unwrap();
        assert_eq!(
            s.checkpoint_resume("sess", "phased").unwrap(),
            Some(NodeId(1))
        );
        // A later checkpoint advances the resume cursor (latest wins).
        s.checkpoint_record("sess", "phased", "p2", NodeId(4))
            .unwrap();
        assert_eq!(
            s.checkpoint_resume("sess", "phased").unwrap(),
            Some(NodeId(4))
        );
        // Scoped per flow_key and per session.
        assert!(s.checkpoint_resume("sess", "other").unwrap().is_none());
        assert!(s.checkpoint_resume("other", "phased").unwrap().is_none());
    }

    #[test]
    fn view_shows_only_visible_and_pinned_symbols() {
        let s = FlowStore::in_memory().unwrap();
        let v = s.put_value("sess", &Value::String("x".into())).unwrap();
        s.bind(
            "sess",
            &SymbolName("a".into()),
            &v,
            Some("Draft"),
            "shown",
            Visibility::Visible,
        )
        .unwrap();
        s.bind(
            "sess",
            &SymbolName("b".into()),
            &v,
            None,
            "hidden one",
            Visibility::Hidden,
        )
        .unwrap();
        s.bind(
            "sess",
            &SymbolName("c".into()),
            &v,
            None,
            "pinned one",
            Visibility::Pinned,
        )
        .unwrap();

        let view = s.view("sess").unwrap();
        let names: Vec<String> = view.symbols.iter().map(|s| s.name.0.clone()).collect();
        assert!(names.contains(&"a".to_string()));
        assert!(names.contains(&"c".to_string()));
        assert!(
            !names.contains(&"b".to_string()),
            "a hidden symbol must not appear in the model-facing view"
        );
        assert!(view.render().contains("$a: Draft = shown"));
    }

    #[test]
    fn run_events_append_and_load_in_order() {
        let s = FlowStore::in_memory().unwrap();
        s.append_event(
            "sess",
            &RunEvent::StepSucceeded {
                step: "s1".into(),
                output: "v_1".into(),
            },
        )
        .unwrap();
        s.append_event(
            "sess",
            &RunEvent::FlowReturned {
                value: "v_1".into(),
            },
        )
        .unwrap();
        let events = s.events("sess").unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], RunEvent::StepSucceeded { .. }));
        assert!(matches!(events[1], RunEvent::FlowReturned { .. }));
    }

    #[test]
    fn byte_budget_accounts_for_stored_values() {
        let s = FlowStore::in_memory().unwrap();
        assert_eq!(s.total_value_bytes("sess").unwrap(), 0);
        s.put_value("sess", &Value::String("some content".into()))
            .unwrap();
        assert!(s.total_value_bytes("sess").unwrap() > 0);
    }

    // ---- C-270: the state port's conformance suite -------------------------------------------

    /// Every durability property the engine relies on, asserted against an arbitrary
    /// [`FlowStateBackend`]. Run once per implementation below, so a non-SQLite backend is held to
    /// exactly the observable behaviour the SQLite one has — this module's invariants: values are
    /// append-only and versioned, symbols are last-writer-wins, and the suspension latch is one-shot.
    fn assert_state_port_conformance(s: &FlowStore) {
        // Values: append-only and versioned — a revision mints a new id and the old one stays
        // addressable. An unknown id is absence, not an error.
        let v1 = s.put_value("sess", &Value::String("one".into())).unwrap();
        let v2 = s.put_value("sess", &Value::String("two".into())).unwrap();
        assert_ne!(v1, v2, "a revision mints a new id");
        assert_eq!(
            s.get_value(&v1).unwrap(),
            Some(Value::String("one".into())),
            "the superseded version stays addressable"
        );
        assert_eq!(s.get_value(&v2).unwrap(), Some(Value::String("two".into())));
        assert!(
            s.get_value(&ValueId("v_999999".into())).unwrap().is_none(),
            "an unknown value id is absence, not an error"
        );

        // Symbols: last-writer-wins over a pointer table, scoped per session.
        let draft = SymbolName("draft".into());
        assert!(
            s.resolve("sess", &draft).unwrap().is_none(),
            "an unbound symbol is absence, not an error"
        );
        s.bind(
            "sess",
            &draft,
            &v1,
            Some("Draft"),
            "first",
            Visibility::Visible,
        )
        .unwrap();
        assert_eq!(s.resolve("sess", &draft).unwrap(), Some(v1.clone()));
        s.bind(
            "sess",
            &draft,
            &v2,
            Some("Draft"),
            "second",
            Visibility::Visible,
        )
        .unwrap();
        assert_eq!(
            s.resolve("sess", &draft).unwrap(),
            Some(v2.clone()),
            "last writer wins"
        );
        assert!(
            s.resolve("other", &draft).unwrap().is_none(),
            "symbols are per-session"
        );

        // The model-facing view projects visible + pinned only.
        s.bind(
            "sess",
            &SymbolName("hid".into()),
            &v1,
            None,
            "h",
            Visibility::Hidden,
        )
        .unwrap();
        s.bind(
            "sess",
            &SymbolName("pin".into()),
            &v1,
            None,
            "p",
            Visibility::Pinned,
        )
        .unwrap();
        let names: Vec<String> = s
            .view("sess")
            .unwrap()
            .symbols
            .iter()
            .map(|sym| sym.name.0.clone())
            .collect();
        assert!(names.contains(&"draft".to_string()));
        assert!(names.contains(&"pin".to_string()));
        assert!(!names.contains(&"hid".to_string()), "hidden stays hidden");

        // Byte accounting rides on the stored values.
        assert!(s.total_value_bytes("sess").unwrap() > 0);
        assert_eq!(
            s.total_value_bytes("empty").unwrap(),
            0,
            "an untouched session accounts for nothing"
        );

        // The suspension latch: at most one per session, a new save replaces the old, a peek does
        // not consume, and a take is one-shot.
        let body = vec![Node::Await {
            binding: Some(SymbolName("x".into())),
            source: "user_input".into(),
            as_type: None,
            condition: None,
        }];
        assert!(!s.has_suspension("sess").unwrap(), "none initially");
        assert!(s.load_suspension("sess").unwrap().is_none());
        s.save_suspension("sess", None, &body, NodeId(3), "user_input")
            .unwrap();
        s.save_suspension("sess", Some("wf"), &body, NodeId(5), "other")
            .unwrap();
        assert!(s.has_suspension("sess").unwrap(), "a peek does not consume");
        let (name, got, node, source) = s.load_suspension("sess").unwrap().expect("a suspension");
        assert_eq!(
            (name.as_deref(), node, source.as_str()),
            (Some("wf"), NodeId(5), "other"),
            "the latest save replaces the earlier one, name included"
        );
        assert_eq!(got, body, "the body round-trips");
        assert!(
            s.has_suspension("sess").unwrap(),
            "load is non-consuming (unlike take)"
        );
        assert!(s.take_suspension("sess").unwrap().is_some());
        assert!(
            s.take_suspension("sess").unwrap().is_none(),
            "take is one-shot"
        );
        s.save_suspension("sess", None, &body, NodeId(1), "again")
            .unwrap();
        s.clear_suspension("sess").unwrap();
        assert!(
            !s.has_suspension("sess").unwrap(),
            "clear removes the latch"
        );

        // Session-scoped composite definitions persist, replace by name, and come back in
        // registration order.
        assert!(s.session_composites("sess").unwrap().is_empty());
        s.save_session_composite("sess", "a", "src-a").unwrap();
        s.save_session_composite("sess", "b", "src-b").unwrap();
        s.save_session_composite("sess", "a", "src-a2").unwrap();
        let composites = s.session_composites("sess").unwrap();
        assert_eq!(composites.len(), 2, "replaced by name, not appended");
        assert!(composites.contains(&("a".to_string(), "src-a2".to_string())));
        assert!(composites.contains(&("b".to_string(), "src-b".to_string())));
        assert!(
            s.session_composites("other").unwrap().is_empty(),
            "composites are per-session"
        );
    }

    // `FlowStore::in_memory` resolves to the SQLite backend only while the feature is on (C-274) —
    // gated so the name cannot quietly start describing `MemoryState`, which its twin below covers.
    #[cfg(feature = "sqlite")]
    #[test]
    fn the_sqlite_backend_conforms_to_the_state_port() {
        assert_state_port_conformance(&FlowStore::in_memory().unwrap());
    }

    /// The acceptance test for C-270: a **non-SQLite** implementation of the port gives identical
    /// observable behaviour. If this and the SQLite twin above both pass, the port is real rather
    /// than a rename.
    #[test]
    fn a_non_sqlite_backend_conforms_to_the_state_port() {
        let store = FlowStore::with_backend(
            Arc::new(MemoryState::default()),
            Arc::new(EventStore::in_memory().unwrap()),
        );
        assert_state_port_conformance(&store);
    }

    /// The port owns its own "no such row" outcome. Five call sites used to match
    /// `rusqlite::Error::QueryReturnedNoRows` structurally; absence now crosses the port as
    /// [`Lookup::NoSuchRow`], a value the trait defines, so a backend with no notion of SQLite errors
    /// can express it.
    #[test]
    fn absence_crosses_the_port_as_its_own_outcome() {
        let backend = MemoryState::default();
        assert_eq!(
            backend.get_value(&ValueId("v_1".into())).unwrap(),
            Lookup::NoSuchRow
        );
        assert_eq!(backend.resolve("sess", "nope").unwrap(), Lookup::NoSuchRow);
        assert_eq!(backend.load_suspension("sess").unwrap(), Lookup::NoSuchRow);
        assert_eq!(backend.take_suspension("sess").unwrap(), Lookup::NoSuchRow);
        assert!(!backend.has_suspension("sess").unwrap());
        // And `Found` is the other arm, not a sentinel value.
        let vid = backend.put_value("sess", "\"x\"", 3, 0).unwrap();
        assert_eq!(
            backend.get_value(&vid).unwrap(),
            Lookup::Found("\"x\"".to_string())
        );
    }

    /// The driver half of the same claim: the SQLite backend reports absence through the port's own
    /// outcome too, never a `rusqlite` error. Its own test since C-274 made the driver optional —
    /// the port's semantics must hold with or without it.
    #[cfg(feature = "sqlite")]
    #[test]
    fn the_sqlite_backend_reports_absence_through_the_port_too() {
        let sqlite = SqliteState::in_memory().unwrap();
        assert_eq!(
            sqlite.get_value(&ValueId("v_1".into())).unwrap(),
            Lookup::NoSuchRow
        );
        assert_eq!(sqlite.resolve("sess", "nope").unwrap(), Lookup::NoSuchRow);
        assert_eq!(sqlite.load_suspension("sess").unwrap(), Lookup::NoSuchRow);
        assert_eq!(sqlite.take_suspension("sess").unwrap(), Lookup::NoSuchRow);
    }
}
