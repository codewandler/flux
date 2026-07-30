//! The engine's **state port**: the storage primitives [`FlowStore`](super::FlowStore) is built on.
//!
//! C-270. `rusqlite` links a C library and cannot build for `wasm32-unknown-unknown`, so an engine
//! that binds it directly cannot be portable. Everything in this module that touched SQL now sits
//! behind [`FlowStateBackend`]; the wrappers, projections, serialization, and the run-event
//! forwarding are implemented once on [`FlowStore`], so a new backend reimplements only what
//! follows. The in-repo precedent is `flux-events`' own `EventBackend`, which solved the same shape
//! for the event log.
//!
//! Two properties of the trait are load-bearing rather than incidental:
//!
//! * **Absence is the port's own outcome.** Every lookup returns [`Lookup`], not a driver error.
//!   Five call sites here used to match `rusqlite::Error::QueryReturnedNoRows` structurally; had
//!   that variant been allowed across the port, a non-SQLite backend would have to fabricate a
//!   SQLite error to say "no such row", and the port would be portable in name only.
//! * **Timestamps come from the caller.** [`FlowStore`] reads the clock once per write and passes
//!   the millisecond stamp in, so a backend never needs a clock of its own. That is also the one
//!   seam an embedder-supplied clock replaces later — `std::time::SystemTime::now` is itself
//!   unavailable on `wasm32-unknown-unknown`.

use flux_core::Result;

use crate::ast::{SymbolName, ValueId, Visibility};

/// The port's own "no such row" outcome — absence as a **value**, never as an error.
///
/// A backend reports a missing row with [`Lookup::NoSuchRow`], so it needs no notion of any
/// particular driver's not-found error. The public [`FlowStore`](super::FlowStore) API converts this
/// to `Option` at its boundary via [`Lookup::found`], which is why none of its signatures changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup<T> {
    /// The row exists, with this content.
    Found(T),
    /// No row matched. Not an error: an unbound symbol, an unknown value id, and a session with no
    /// pending suspension are all ordinary states.
    NoSuchRow,
}

impl<T> Lookup<T> {
    /// The found row, or `None` for [`Lookup::NoSuchRow`] — the bridge to the `Option`-returning
    /// public store API.
    pub fn found(self) -> Option<T> {
        match self {
            Lookup::Found(row) => Some(row),
            Lookup::NoSuchRow => None,
        }
    }

    /// Whether a row matched.
    pub fn is_found(&self) -> bool {
        matches!(self, Lookup::Found(_))
    }

    /// Apply `f` to a found row, leaving [`Lookup::NoSuchRow`] alone.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Lookup<U> {
        match self {
            Lookup::Found(row) => Lookup::Found(f(row)),
            Lookup::NoSuchRow => Lookup::NoSuchRow,
        }
    }
}

impl<T> From<Option<T>> for Lookup<T> {
    fn from(row: Option<T>) -> Self {
        match row {
            Some(row) => Lookup::Found(row),
            None => Lookup::NoSuchRow,
        }
    }
}

/// Where a symbol points and how it projects — one write to the last-writer-wins symbol table.
/// Grouped into a struct so [`FlowStateBackend::bind`] takes a row rather than seven positional
/// arguments.
#[derive(Debug, Clone, Copy)]
pub struct SymbolBinding<'a> {
    /// The value the name points at from now on.
    pub value_id: &'a str,
    /// The optional type hint shown in the model-facing view.
    pub ty: Option<&'a str>,
    /// The one-line summary the view renders instead of the value.
    pub summary: &'a str,
    /// The projection policy — only [`Visibility::is_shown`] symbols reach the view.
    pub visibility: Visibility,
}

/// One symbol as a backend returns it: **unfiltered**, because the projection policy belongs to
/// [`FlowStore::view`](super::FlowStore::view) and not to storage. Deliberately a distinct type from
/// the model-facing [`SymbolView`](flux_lang::store::SymbolView) for exactly that reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSymbol {
    /// The bound name.
    pub name: SymbolName,
    /// The optional type hint.
    pub ty: Option<String>,
    /// The one-line summary.
    pub summary: String,
    /// The stored projection policy. A tag a backend cannot decode is reported as
    /// [`Visibility::Hidden`] — the conservative arm, so an unreadable row never leaks into the view.
    pub visibility: Visibility,
}

/// A session's pending continuation: the one-shot suspension latch, as it sits in storage.
///
/// `body` is the serialized flow body and is **opaque** to the backend —
/// [`FlowStore`](super::FlowStore) owns encoding it and owns recovering from a body that no longer
/// deserializes. A backend stores and returns those bytes unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suspension {
    /// The suspended flow's declared name, if it had one. Part of the resume point on purpose: a
    /// named flow's checkpoint `flow_key` is name + body hash, so a resume must carry the name to
    /// read and record checkpoints under the key the original run used (L-21).
    pub flow_name: Option<String>,
    /// The serialized flow body.
    pub body: String,
    /// The suspended `await` node's top-level index.
    pub node: i64,
    /// The awaited input source.
    pub source: String,
}

/// The storage primitives every state backend provides — exactly the operations that used to touch
/// SQL directly.
///
/// Object-safe (all `&self`, owned returns) so [`FlowStore`](super::FlowStore) can hold an
/// `Arc<dyn FlowStateBackend>`. `Send + Sync` because one store is shared across the engine's tasks.
///
/// The invariants a backend must preserve are this module's, not SQLite's: **values are append-only
/// and versioned** (a revision mints a new [`ValueId`] and the old one stays addressable), **symbols
/// are last-writer-wins**, and **the suspension latch is at most one per session** — a save replaces
/// any prior, [`load_suspension`](Self::load_suspension) does not consume, and
/// [`take_suspension`](Self::take_suspension) is one-shot and must load-and-delete atomically.
/// [`FlowStore`](super::FlowStore)'s test module carries the conformance suite that holds an
/// implementation to all of them.
pub trait FlowStateBackend: Send + Sync {
    // ---- values: append-only and versioned -----------------------------------------------------

    /// Store `data` (already-serialized value JSON) and return its freshly minted id. Append-only:
    /// this never overwrites an earlier value, and the backend owns the id format.
    fn put_value(
        &self,
        session_id: &str,
        data: &str,
        bytes: i64,
        created_at_ms: i64,
    ) -> Result<ValueId>;

    /// The stored value JSON for `id`, or [`Lookup::NoSuchRow`].
    fn get_value(&self, id: &ValueId) -> Result<Lookup<String>>;

    /// Total stored value bytes for a session — the budget-accounting surface. `0` for a session
    /// that has stored nothing.
    fn total_value_bytes(&self, session_id: &str) -> Result<u64>;

    // ---- symbols: a last-writer-wins pointer table ---------------------------------------------

    /// Point `name` at `binding` for this session, creating the row or overwriting it in place. The
    /// previously pointed-at value stays stored.
    fn bind(
        &self,
        session_id: &str,
        name: &str,
        binding: SymbolBinding<'_>,
        updated_at_ms: i64,
    ) -> Result<()>;

    /// The value id `name` currently points at, or [`Lookup::NoSuchRow`] when it is unbound.
    fn resolve(&self, session_id: &str, name: &str) -> Result<Lookup<String>>;

    /// Every symbol bound in this session, **newest-updated first** then by name ascending — the
    /// order the model-facing view is projected in. Returns all visibilities; the caller applies the
    /// projection policy.
    fn symbols(&self, session_id: &str) -> Result<Vec<StoredSymbol>>;

    // ---- the one-shot suspension latch ---------------------------------------------------------

    /// Persist `suspension` as this session's pending continuation, replacing any prior one.
    fn save_suspension(
        &self,
        session_id: &str,
        suspension: &Suspension,
        created_at_ms: i64,
    ) -> Result<()>;

    /// Read the pending continuation **without** consuming it.
    fn load_suspension(&self, session_id: &str) -> Result<Lookup<Suspension>>;

    /// Read **and** remove the pending continuation, atomically — the one-shot resume point. A
    /// backend must not admit an interleaving in which two callers both observe the same latch.
    fn take_suspension(&self, session_id: &str) -> Result<Lookup<Suspension>>;

    /// Whether a pending continuation exists — a non-consuming existence check.
    fn has_suspension(&self, session_id: &str) -> Result<bool>;

    /// Drop the pending continuation, if any. Clearing an empty latch is not an error.
    fn clear_suspension(&self, session_id: &str) -> Result<()>;

    // ---- session-scoped composite op definitions -----------------------------------------------

    /// Register (or replace, by name) a session-scoped composite op definition, as normalized
    /// Flux-Lang source.
    fn save_session_composite(
        &self,
        session_id: &str,
        name: &str,
        source: &str,
        updated_at_ms: i64,
    ) -> Result<()>;

    /// Every composite definition registered for this session as `(name, source)`, oldest
    /// registration first then by name ascending.
    fn session_composites(&self, session_id: &str) -> Result<Vec<(String, String)>>;
}
