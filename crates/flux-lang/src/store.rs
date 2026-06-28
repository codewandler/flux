//! The value/symbol/event **store** the interpreter reads and writes through. The engine backs it with
//! a durable SQLite store (`flux_flow::state::FlowStore`); the language ships an in-memory [`MemStore`]
//! so the interpreter can run standalone (CLI, tests) without a database.
//!
//! [`SessionView`] is the compact, model-facing projection of a session's symbols — what a planner is
//! shown instead of re-sent raw outputs.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use flux_core::Result;

use crate::ast::{NodeId, RunEvent, SymbolName, Value, ValueId, Visibility};

/// One symbol as projected into the model-facing view: a name, a one-line summary, an optional type
/// hint, and its visibility. The raw value is never included — only the runtime dereferences it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SymbolView {
    pub name: SymbolName,
    pub ty: Option<String>,
    pub summary: String,
    pub visibility: Visibility,
}

/// The compact, policy-filtered projection of a session's symbols — what the model sees instead of
/// re-sent raw outputs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionView {
    pub symbols: Vec<SymbolView>,
}

impl SessionView {
    /// Render the view as compact lines, e.g. `$draft: Draft = Renewal follow-up`.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for s in &self.symbols {
            out.push('$');
            out.push_str(&s.name.0);
            if let Some(ty) = &s.ty {
                out.push_str(": ");
                out.push_str(ty);
            }
            out.push_str(" = ");
            out.push_str(&s.summary);
            out.push('\n');
        }
        out
    }
}

/// The store the interpreter resolves symbols to values through. Implementations own persistence; the
/// language defines only the contract. Must be `Send + Sync` (the interpreter holds it across `await`).
pub trait ValueStore: Send + Sync {
    /// Store an immutable value, returning its id.
    fn put_value(&self, session_id: &str, value: &Value) -> Result<ValueId>;
    /// Read a stored value by id.
    fn get_value(&self, id: &ValueId) -> Result<Option<Value>>;
    /// Bind a symbol name to a stored value id (with a one-line summary for the view).
    fn bind(
        &self,
        session_id: &str,
        name: &SymbolName,
        vid: &ValueId,
        ty: Option<&str>,
        summary: &str,
        visibility: Visibility,
    ) -> Result<()>;
    /// Resolve a symbol name to its bound value id, if any.
    fn resolve(&self, session_id: &str, name: &SymbolName) -> Result<Option<ValueId>>;
    /// Append a run-event to the session's trace.
    fn append_event(&self, session_id: &str, event: &RunEvent) -> Result<()>;
    /// The model-facing projection of the session's symbols.
    fn view(&self, session_id: &str) -> Result<SessionView>;
    /// The binding metadata (type hint, summary, visibility) for one symbol, if bound. The
    /// `ctx`/`ctx_append` budget reads visibility through this to shrink a pack by tier. The default
    /// derives it from [`view`](Self::view); a store may override for efficiency.
    fn binding(&self, session_id: &str, name: &SymbolName) -> Result<Option<SymbolView>> {
        Ok(self
            .view(session_id)?
            .symbols
            .into_iter()
            .find(|s| &s.name == name))
    }

    /// The durable, cross-run keyed state this store backs, if any. Default `None` — a store with no
    /// durability (a throwaway/standalone interpreter) makes `once` run every time and `checkpoint`
    /// a no-op. The engine's persistent store overrides this to return `Some(self)`, mirroring the
    /// optional-capability shape of the engine's other host seams.
    fn as_durable(&self) -> Option<&dyn DurableStore> {
        None
    }
}

/// A completed `once` (effect-level memo) record: the value the body bound (if any) plus a one-line
/// summary, so a re-run can skip the side effect and rebind the value.
#[derive(Debug, Clone, PartialEq)]
pub struct OnceRecord {
    pub value: Option<ValueId>,
    pub summary: String,
}

/// Durable, cross-run keyed state for the side-effect-safety primitives. Keyed by
/// `(session_id, label)` — the label is the explicit idempotency key (matching `memo`/`await`, which
/// key on explicit `name`/`source` strings). Both the engine's persistent store and the in-memory
/// [`MemStore`] implement this by folding over their append-only run-event log, so history is never
/// rewritten ([[event-store-unification]] in spirit).
pub trait DurableStore: Send + Sync {
    /// The recorded completion for `(session, label)`, if a prior run finished the `once` body
    /// successfully. `Some` means "already done — skip the side effect and reuse the value".
    fn once_lookup(&self, session_id: &str, label: &str) -> Result<Option<OnceRecord>>;
    /// Record a `once` block that completed **successfully**. Append-only; only ever called on
    /// success, so a failed body leaves no record and a later re-run retries.
    fn once_complete(
        &self,
        session_id: &str,
        label: &str,
        value: Option<&ValueId>,
        summary: &str,
    ) -> Result<()>;

    /// The furthest top-level index a `checkpoint` reached for `(session, flow_key)` on a prior run.
    /// A fresh re-run fast-forwards past it — the prefix's symbols are already durably bound and its
    /// side effects are not repeated. `None` means no checkpoint has been recorded for this flow yet.
    fn checkpoint_resume(&self, session_id: &str, flow_key: &str) -> Result<Option<NodeId>>;
    /// Record that a `checkpoint` was reached at top-level `index` for `(session, flow_key)`.
    /// Append-only.
    fn checkpoint_record(
        &self,
        session_id: &str,
        flow_key: &str,
        label: &str,
        index: NodeId,
    ) -> Result<()>;
}

/// A simple in-memory [`ValueStore`] — no persistence, no budgeting, no event retention beyond the
/// process. Lets the interpreter run without a database (the `fluxlang` CLI, tests). Not a session
/// store for production use.
#[derive(Default)]
pub struct MemStore {
    inner: Mutex<MemInner>,
}

#[derive(Default)]
struct MemInner {
    values: HashMap<String, Value>,
    /// `(session, symbol) -> value_id`.
    symbols: HashMap<(String, String), String>,
    /// `(session, symbol) -> (ty, summary, visibility)` for the view.
    meta: HashMap<(String, String), (Option<String>, String, Visibility)>,
    events: HashMap<String, Vec<RunEvent>>,
    next: u64,
}

impl MemStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// The recorded run-events for a session (in append order).
    pub fn events(&self, session_id: &str) -> Vec<RunEvent> {
        self.inner
            .lock()
            .unwrap()
            .events
            .get(session_id)
            .cloned()
            .unwrap_or_default()
    }
}

impl ValueStore for MemStore {
    fn put_value(&self, _session_id: &str, value: &Value) -> Result<ValueId> {
        let mut g = self.inner.lock().unwrap();
        g.next += 1;
        let id = format!("v{}", g.next);
        g.values.insert(id.clone(), value.clone());
        Ok(ValueId(id))
    }

    fn get_value(&self, id: &ValueId) -> Result<Option<Value>> {
        Ok(self.inner.lock().unwrap().values.get(&id.0).cloned())
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
        let mut g = self.inner.lock().unwrap();
        let key = (session_id.to_string(), name.0.clone());
        g.symbols.insert(key.clone(), vid.0.clone());
        g.meta.insert(
            key,
            (ty.map(str::to_string), summary.to_string(), visibility),
        );
        Ok(())
    }

    fn resolve(&self, session_id: &str, name: &SymbolName) -> Result<Option<ValueId>> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .symbols
            .get(&(session_id.to_string(), name.0.clone()))
            .map(|s| ValueId(s.clone())))
    }

    fn append_event(&self, session_id: &str, event: &RunEvent) -> Result<()> {
        self.inner
            .lock()
            .unwrap()
            .events
            .entry(session_id.to_string())
            .or_default()
            .push(event.clone());
        Ok(())
    }

    fn view(&self, session_id: &str) -> Result<SessionView> {
        let g = self.inner.lock().unwrap();
        let symbols = g
            .meta
            .iter()
            .filter(|((s, _), _)| s == session_id)
            .map(|((_, name), (ty, summary, vis))| SymbolView {
                name: SymbolName(name.clone()),
                ty: ty.clone(),
                summary: summary.clone(),
                visibility: *vis,
            })
            .collect();
        Ok(SessionView { symbols })
    }

    fn as_durable(&self) -> Option<&dyn DurableStore> {
        Some(self)
    }
}

/// In-memory durability: fold over the session's retained run-events (the same append-only basis the
/// engine's persistent store uses), so the standalone interpreter exercises the `once` durable path.
impl DurableStore for MemStore {
    fn once_lookup(&self, session_id: &str, label: &str) -> Result<Option<OnceRecord>> {
        Ok(self
            .events(session_id)
            .into_iter()
            .rev()
            .find_map(|e| match e {
                RunEvent::OnceCompleted {
                    label: l,
                    value,
                    summary,
                } if l == label => Some(OnceRecord { value, summary }),
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
            .events(session_id)
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
