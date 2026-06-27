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

use crate::ast::{RunEvent, SymbolName, Value, ValueId, Visibility};

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
}
