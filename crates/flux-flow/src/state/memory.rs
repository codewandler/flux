//! A dependency-free [`FlowStateBackend`]: plain collections behind one lock, no driver, no C
//! library, no filesystem.
//!
//! This exists to make the port falsifiable. `FlowStore::in_memory` is *SQLite* in memory — it still
//! links `rusqlite` — so it cannot demonstrate that the engine runs on anything else. `MemoryState`
//! can, and the conformance suite in [`super`] holds it to exactly the observable behaviour
//! [`SqliteState`](super::SqliteState) has. It is also the shape a `wasm32` embedder's backend takes:
//! nothing here needs a syscall.
//!
//! It is **not** durable and is not a production store. Native flux keeps SQLite; this is the
//! reference implementation of the port and the substrate for tests that must not touch a driver.

use std::collections::BTreeMap;
use std::sync::Mutex;

use flux_core::Result;

use crate::ast::{SymbolName, ValueId, Visibility};
use crate::state::port::{FlowStateBackend, Lookup, StoredSymbol, Suspension, SymbolBinding};

/// One stored value: append-only, so a row is written once and never updated.
struct ValueRow {
    session_id: String,
    data: String,
    bytes: i64,
}

/// One symbol-table row — overwritten in place on rebind (last-writer-wins).
struct SymbolRow {
    value_id: String,
    ty: Option<String>,
    summary: String,
    visibility: Visibility,
    updated_at_ms: i64,
}

/// One session-scoped composite definition.
struct CompositeRow {
    source: String,
    updated_at_ms: i64,
}

/// Everything the backend owns, under a single lock — the same coarse granularity
/// [`SqliteState`](super::SqliteState) gets from its `Mutex<Connection>`, so `take_suspension` is
/// atomic here for the same reason it is there.
#[derive(Default)]
struct Tables {
    /// Keyed by the minted value id.
    values: BTreeMap<String, ValueRow>,
    /// The last id minted; ids are `v_<n>` from 1, matching SQLite's rowid sequence.
    minted: i64,
    /// Keyed by `(session_id, name)`.
    symbols: BTreeMap<(String, String), SymbolRow>,
    /// At most one pending continuation per session.
    suspensions: BTreeMap<String, Suspension>,
    /// Keyed by `(session_id, name)`.
    composites: BTreeMap<(String, String), CompositeRow>,
}

/// The non-SQLite reference implementation of the engine's state port.
#[derive(Default)]
pub struct MemoryState {
    tables: Mutex<Tables>,
}

impl MemoryState {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl FlowStateBackend for MemoryState {
    fn put_value(
        &self,
        session_id: &str,
        data: &str,
        bytes: i64,
        _created_at_ms: i64,
    ) -> Result<ValueId> {
        let mut t = self.tables.lock().unwrap();
        t.minted += 1;
        let id = ValueId(format!("v_{}", t.minted));
        t.values.insert(
            id.0.clone(),
            ValueRow {
                session_id: session_id.to_string(),
                data: data.to_string(),
                bytes,
            },
        );
        Ok(id)
    }

    /// An id this store never minted is [`Lookup::NoSuchRow`] — including a malformed one. (The
    /// SQLite backend rejects a malformed id as an error, because it has to parse a rowid out of it;
    /// that distinction is a property of *its* id format, not of the port.)
    fn get_value(&self, id: &ValueId) -> Result<Lookup<String>> {
        let t = self.tables.lock().unwrap();
        Ok(t.values.get(&id.0).map(|v| v.data.clone()).into())
    }

    fn total_value_bytes(&self, session_id: &str) -> Result<u64> {
        let t = self.tables.lock().unwrap();
        let sum: i64 = t
            .values
            .values()
            .filter(|v| v.session_id == session_id)
            .map(|v| v.bytes)
            .sum();
        Ok(sum.max(0) as u64)
    }

    fn bind(
        &self,
        session_id: &str,
        name: &str,
        binding: SymbolBinding<'_>,
        updated_at_ms: i64,
    ) -> Result<()> {
        let mut t = self.tables.lock().unwrap();
        t.symbols.insert(
            (session_id.to_string(), name.to_string()),
            SymbolRow {
                value_id: binding.value_id.to_string(),
                ty: binding.ty.map(str::to_string),
                summary: binding.summary.to_string(),
                visibility: binding.visibility,
                updated_at_ms,
            },
        );
        Ok(())
    }

    fn resolve(&self, session_id: &str, name: &str) -> Result<Lookup<String>> {
        let t = self.tables.lock().unwrap();
        Ok(t.symbols
            .get(&(session_id.to_string(), name.to_string()))
            .map(|s| s.value_id.clone())
            .into())
    }

    fn symbols(&self, session_id: &str) -> Result<Vec<StoredSymbol>> {
        let t = self.tables.lock().unwrap();
        let mut rows: Vec<(&String, &SymbolRow)> = t
            .symbols
            .iter()
            .filter(|((sid, _), _)| sid == session_id)
            .map(|((_, name), row)| (name, row))
            .collect();
        // The SQLite backend's `ORDER BY updated_at DESC, name ASC`, reproduced exactly — the view's
        // "newest first" ordering is observable, so it is part of the port's contract.
        rows.sort_by(|(a_name, a), (b_name, b)| {
            b.updated_at_ms
                .cmp(&a.updated_at_ms)
                .then_with(|| a_name.cmp(b_name))
        });
        Ok(rows
            .into_iter()
            .map(|(name, row)| StoredSymbol {
                name: SymbolName(name.clone()),
                ty: row.ty.clone(),
                summary: row.summary.clone(),
                visibility: row.visibility,
            })
            .collect())
    }

    fn save_suspension(
        &self,
        session_id: &str,
        suspension: &Suspension,
        _created_at_ms: i64,
    ) -> Result<()> {
        let mut t = self.tables.lock().unwrap();
        // At most one per session: a save replaces any prior.
        t.suspensions
            .insert(session_id.to_string(), suspension.clone());
        Ok(())
    }

    fn load_suspension(&self, session_id: &str) -> Result<Lookup<Suspension>> {
        let t = self.tables.lock().unwrap();
        Ok(t.suspensions.get(session_id).cloned().into())
    }

    fn take_suspension(&self, session_id: &str) -> Result<Lookup<Suspension>> {
        // One-shot: read and remove under the same lock.
        let mut t = self.tables.lock().unwrap();
        Ok(t.suspensions.remove(session_id).into())
    }

    fn has_suspension(&self, session_id: &str) -> Result<bool> {
        let t = self.tables.lock().unwrap();
        Ok(t.suspensions.contains_key(session_id))
    }

    fn clear_suspension(&self, session_id: &str) -> Result<()> {
        let mut t = self.tables.lock().unwrap();
        t.suspensions.remove(session_id);
        Ok(())
    }

    fn save_session_composite(
        &self,
        session_id: &str,
        name: &str,
        source: &str,
        updated_at_ms: i64,
    ) -> Result<()> {
        let mut t = self.tables.lock().unwrap();
        t.composites.insert(
            (session_id.to_string(), name.to_string()),
            CompositeRow {
                source: source.to_string(),
                updated_at_ms,
            },
        );
        Ok(())
    }

    fn session_composites(&self, session_id: &str) -> Result<Vec<(String, String)>> {
        let t = self.tables.lock().unwrap();
        let mut rows: Vec<(&String, &CompositeRow)> = t
            .composites
            .iter()
            .filter(|((sid, _), _)| sid == session_id)
            .map(|((_, name), row)| (name, row))
            .collect();
        // `ORDER BY updated_at ASC, name ASC` — registration order.
        rows.sort_by(|(a_name, a), (b_name, b)| {
            a.updated_at_ms
                .cmp(&b.updated_at_ms)
                .then_with(|| a_name.cmp(b_name))
        });
        Ok(rows
            .into_iter()
            .map(|(name, row)| (name.clone(), row.source.clone()))
            .collect())
    }
}
