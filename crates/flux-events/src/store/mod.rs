//! The append-only event store.
//!
//! One ordered log holds every fact; a small `streams` registry mints the `s_<n>` session ids and
//! serves the session-list read model (it is rebuildable from the log). A *stream* is one session,
//! so messages, run events, and turn telemetry interleave in one causal order — the whole point of
//! unifying the three old logs.
//!
//! [`EventStore`] is a thin façade over an internal [`Backend`] enum: the ~20 storage primitives
//! ([`EventBackend`]) delegate per-backend, while every wrapper, projection, and serde decode lives
//! here, backend-neutral, over the shared [`RawEvent`] row tuple. The default backend is embedded
//! SQLite ([`sqlite::SqliteEvents`]); a Postgres backend plugs into the same seam behind a feature.
//! There is deliberately **no public trait** — 23 consumer files hold `Arc<EventStore>` concretely,
//! so the public API stays a concrete struct with a byte-identical surface across backends.
//!
//! C-274: the SQLite backend is a **feature** (`sqlite`, on by default), because its driver links a
//! C library and so cannot build for `wasm32-unknown-unknown`. With it off, the driver-free
//! [`ephemeral::EphemeralEvents`] is the backend — always compiled, held to the same conformance
//! suite — so a portable build still has a *usable* store rather than merely a compiling one. Since
//! `EventBackend` is crate-private on purpose, an embedder could not otherwise supply one.

#[cfg(feature = "postgres")]
mod postgres;
#[cfg(feature = "sqlite")]
mod sqlite;

mod ephemeral;

// Only the SQLite backend takes a filesystem path — see `EventStore::open`.
#[cfg(feature = "sqlite")]
use std::path::Path;

use flux_core::{Error, Message, Result, Usage};
use flux_lang::ast::RunEvent;

use crate::context::EventContext;
use crate::kind::{EventKind, NewEvent, StoredEvent};
use crate::memory::{self, MemoryEntry, MemoryNote, MemoryScope, Receipt};
use crate::projection;

use ephemeral::EphemeralEvents;
#[cfg(feature = "sqlite")]
use sqlite::SqliteEvents;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Parse a session id (`"s_<n>"`) into its registry rowid, matching the old `s_<rowid>`
/// scheme so `FlowStore`'s `session_id`-keyed tables keep resolving.
fn parse_id(id: &str) -> Result<i64> {
    id.strip_prefix("s_")
        .and_then(|n| n.parse::<i64>().ok())
        .ok_or_else(|| Error::Other(format!("invalid session id: {id:?}")))
}

/// Raw event columns as read from a row, before the `payload` JSON is decoded. Backend-neutral:
/// every backend's load primitives produce these tuples, and [`decode_all`] turns them into
/// [`StoredEvent`]s in one shared place — so the serde contract can never drift between backends.
type RawEvent = (i64, i64, String, u32, i64, String, Option<i64>);

/// Decode a batch of raw rows (all from `stream`, all sharing its run `ctx`) into [`StoredEvent`]s.
/// The single serde-decode point, shared by every backend.
///
/// C-81 (upgrade/forward-compat): a single **undecodable** row — an event variant written by a
/// newer build ([`EventKind`] is a deliberately *closed* enum, so an unknown `kind` tag has no
/// arm), or a payload that got corrupted on disk — is **skipped and logged**, never
/// `?`-propagated. Aborting the whole batch would make `conversation`/`turns`/`observations`/
/// `load_stream` fail for the *entire* stream (conversations unreadable, turns aborting) through a
/// rolling-upgrade window or on any single corrupt byte. Skipping keeps every good event readable.
///
/// Why skip-and-log rather than an inert `#[serde(other)] Unknown` variant: (1) it also covers a
/// *corrupt* payload (a known tag with malformed data — which `#[serde(other)]` would still fail
/// to decode), not just unknown variants; (2) it keeps [`EventKind`] closed, preserving the
/// exhaustive-match guarantee every projection relies on (see the `kind` module header) — an
/// `Unknown` variant would force a new arm onto every match and defeat that guarantee. The
/// trade-off: a skipped row is invisible to readers until the reader is upgraded to a build that
/// understands it (it stays on disk, append-only — nothing is deleted), and the drop is surfaced
/// only to stderr, not through the `Result`.
fn decode_all(stream: &str, ctx: &EventContext, raw: Vec<RawEvent>) -> Result<Vec<StoredEvent>> {
    let mut out = Vec::with_capacity(raw.len());
    #[cfg(test)]
    let mut decoded_tags: Vec<&'static str> = Vec::new();
    for (global_seq, stream_seq, id, schema_version, ts, payload, turn_id) in raw {
        let kind: EventKind = match serde_json::from_str(&payload) {
            Ok(kind) => kind,
            Err(e) => {
                eprintln!(
                    "flux-events: skipping undecodable event row (unknown variant or corrupt \
                     payload) in stream {stream} at global_seq {global_seq} (id {id}): {e}"
                );
                continue;
            }
        };
        #[cfg(test)]
        decoded_tags.push(kind.kind_tag());
        out.push(StoredEvent {
            global_seq,
            stream: stream.to_string(),
            stream_seq,
            id,
            turn_id,
            schema_version,
            ts_ms: ts,
            kind,
            context: ctx.clone(),
        });
    }
    #[cfg(test)]
    DECODED_KINDS.with(|seen| seen.borrow_mut().extend(decoded_tags));
    Ok(out)
}

#[cfg(test)]
thread_local! {
    /// Test-only instrument: the `kind_tag()` of every event row decoded **on this thread**.
    ///
    /// A read that claims to be kind-filtered (`conversation`, `observations`, and A-100's
    /// `SessionLog::open`) has no other honest witness — the filter lives in SQL, so the only way
    /// to prove a large stream's bulky plan/run/usage payloads were never fetched *or decoded* is
    /// to observe what actually reached the decoder. Thread-local, so tests running in parallel
    /// never see each other's decodes.
    pub(crate) static DECODED_KINDS: std::cell::RefCell<Vec<&'static str>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Run `f`, returning its value alongside the event kinds the store decoded while it ran.
#[cfg(test)]
pub(crate) fn decoded_kinds_during<T>(f: impl FnOnce() -> T) -> (T, Vec<&'static str>) {
    DECODED_KINDS.with(|seen| seen.borrow_mut().clear());
    let out = f();
    (out, DECODED_KINDS.with(|seen| seen.borrow().clone()))
}

/// Metadata about a session, projected from its events. (The session registry view —
/// "stream" and "session" are the same thing here.)
#[derive(Debug, Clone, PartialEq)]
pub struct SessionInfo {
    pub id: String,
    pub model: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    /// The owning run's tenant/agent context (empty for single-tenant sessions).
    pub context: EventContext,
}

/// One source event queued for atomic re-import by [`EventBackend::copy_session_atomic`] (D-185):
/// the event's payload/timestamp plus enough of its ORIGINAL identity (`old_global_seq`, `id`) to
/// remap turn scoping against the freshly-minted destination ids — `old_global_seq` is the key a
/// copied `TurnStarted` populates in the map, `turn_id` (also an old-store global_seq) is looked up
/// in it.
pub(crate) struct CopyEvent {
    pub kind: EventKind,
    pub schema_version: u32,
    pub ts_ms: i64,
    pub turn_id: Option<i64>,
    pub old_global_seq: i64,
    /// The source event's stable id, for a readable diagnostic on an unmappable `turn_id`.
    pub id: String,
}

/// A one-line session summary for listings (`flux sessions` / the REPL `/sessions`).
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSummary {
    pub id: String,
    pub model: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    /// Length of the current (post-compaction) conversation — kept equal to
    /// `conversation(id).len()` by the registry, so the count never disagrees with a replay.
    pub messages: usize,
    /// The owning run's tenant/agent context (empty for single-tenant sessions).
    pub context: EventContext,
}

/// A session-search filter (C-164): narrowing predicates for [`EventStore::search`] — free-text
/// over the session's conversation, a touched-file path, and/or a date range. Each predicate is
/// evaluated as a **projection over the existing event log** (`list`/`conversation`/
/// `observations`, the same backend-neutral primitives every other read uses) — adding a filter
/// here never means adding SQL, a new table, or a persisted index.
///
/// `#[non_exhaustive]`: this crate is published (`codewandler-flux-events`), and a future story is
/// likely to add another predicate (author, model, …). Construct with [`SessionFilter::new`] (or
/// `Default`) and the `with_*` builders, so growing this struct stays additive, not breaking.
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct SessionFilter {
    /// Case-insensitive substring match over the session's conversation text.
    pub query: Option<String>,
    /// A path a tool call in this session touched, matched at a path boundary in either direction
    /// (a relative filter like `main.rs` matches a recorded absolute subject like
    /// `/repo/src/main.rs`, and vice versa) — never a raw substring, so `rc/main.rs` does not
    /// match `src/main.rs`.
    pub file: Option<String>,
    /// Only sessions whose activity window (`created_at_ms..=updated_at_ms`) overlaps this
    /// inclusive lower bound (epoch ms).
    pub since_ms: Option<i64>,
    /// … overlaps this inclusive upper bound (epoch ms).
    pub until_ms: Option<i64>,
}

impl SessionFilter {
    /// An empty filter — equivalent to [`EventStore::list`] when passed to
    /// [`EventStore::search`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Match sessions whose conversation contains `query` (case-insensitive).
    pub fn with_query(mut self, query: impl Into<String>) -> Self {
        self.query = Some(query.into());
        self
    }

    /// Match sessions that recorded a tool call touching `file`.
    pub fn with_file(mut self, file: impl Into<String>) -> Self {
        self.file = Some(file.into());
        self
    }

    /// Match sessions active at or after `since_ms` (epoch ms, inclusive).
    pub fn with_since_ms(mut self, since_ms: i64) -> Self {
        self.since_ms = Some(since_ms);
        self
    }

    /// Match sessions active at or before `until_ms` (epoch ms, inclusive).
    pub fn with_until_ms(mut self, until_ms: i64) -> Self {
        self.until_ms = Some(until_ms);
        self
    }

    /// True when no predicate is set — [`EventStore::search`] treats this identically to
    /// [`EventStore::list`].
    pub fn is_empty(&self) -> bool {
        self.query.is_none()
            && self.file.is_none()
            && self.since_ms.is_none()
            && self.until_ms.is_none()
    }
}

/// Path-boundary match used by [`EventStore::search`]'s `file` predicate: equal, or one is a
/// `/`-bounded path suffix of the other. Deliberately NOT a raw substring test — `rc/main.rs` must
/// not match a recorded subject `src/main.rs` just because the characters appear in sequence.
fn path_matches(subject: &str, filter: &str) -> bool {
    if subject == filter {
        return true;
    }
    let ends_at_boundary = |long: &str, short: &str| {
        long.len() > short.len()
            && long.ends_with(short)
            && long.as_bytes()[long.len() - short.len() - 1] == b'/'
    };
    ends_at_boundary(subject, filter) || ends_at_boundary(filter, subject)
}

/// The storage primitives every backend provides. These are exactly the methods that touch SQL;
/// every wrapper (`record_*`, `begin_turn`, …) and projection (`conversation`, `cost_summary_all`,
/// …) is implemented once on [`EventStore`] over these, so a new backend only reimplements this
/// trait. Object-safe (all `&self`, owned args/returns) so [`Backend`] can dispatch through
/// `&dyn EventBackend`.
///
/// The read primitives return fully-decoded [`StoredEvent`]s (each produces [`RawEvent`] tuples its
/// own way, then calls the shared [`decode_all`]); the write primitives own their transaction.
trait EventBackend: Send + Sync {
    fn create_session_with_context(&self, model: &str, ctx: &EventContext) -> Result<String>;
    fn latest_session(&self) -> Result<Option<String>>;
    fn info(&self, stream: &str) -> Result<SessionInfo>;
    fn list(&self, limit: usize) -> Result<Vec<SessionSummary>>;
    fn list_for_account(&self, account: &str, limit: usize) -> Result<Vec<SessionSummary>>;
    fn account_streams(&self, account: &str) -> Result<Vec<String>>;
    fn find_correlated(&self, correlation_id: &str, agent_id: &str) -> Result<Option<String>>;
    fn find_correlated_in_realm(
        &self,
        correlation_id: &str,
        agent_id: &str,
        realm: &str,
    ) -> Result<Option<String>>;
    fn children_of(&self, stream: &str) -> Result<Vec<String>>;
    fn prune_empty(&self) -> Result<usize> {
        self.prune_empty_excluding(&[])
    }
    fn prune_empty_excluding(&self, keep: &[String]) -> Result<usize>;
    fn prune_inactive(&self, agent_id: &str, cutoff_ms: i64) -> Result<usize> {
        self.prune_inactive_excluding(agent_id, cutoff_ms, &[])
    }
    /// [`prune_inactive`](Self::prune_inactive) with a keep-list: a stream in `keep` is never
    /// pruned even when expired. The A2A surface passes its in-process live-task ids (A-54): a
    /// task queued behind the single-turn gate has a frozen `updated_at`, so without the
    /// exclusion a long queue could look expired to a concurrent request's mint-time sweep —
    /// the C-29 hazard, re-opened by non-blocking sends minting before the gate.
    fn prune_inactive_excluding(
        &self,
        agent_id: &str,
        cutoff_ms: i64,
        keep: &[String],
    ) -> Result<usize>;
    fn append(&self, stream: &str, ev: NewEvent) -> Result<StoredEvent>;
    /// A-100: append `ev` **only if** the stream's newest message-affecting event
    /// (`kind IN ('message','compacted')`) is still exactly `expected_seq` (`-1` when the stream
    /// has none) — the compare-and-append behind [`SessionLog`](crate::SessionLog)'s turn
    /// transitions. The guard is read inside the same write transaction as the insert, so a
    /// concurrent writer cannot slip a message in between a handle deriving its tail and writing
    /// against it. `Ok(None)` means the guard missed and **nothing** was appended.
    fn append_if_conversation_head(
        &self,
        stream: &str,
        ev: NewEvent,
        expected_seq: i64,
    ) -> Result<Option<StoredEvent>>;
    fn load_stream(&self, stream: &str, after_seq: Option<i64>) -> Result<Vec<StoredEvent>>;
    fn load_by_kind(&self, stream: &str, kind: &str) -> Result<Vec<StoredEvent>>;
    fn conversation_delta(&self, stream: &str, after_seq: i64) -> Result<Vec<StoredEvent>>;
    fn load_turn(&self, stream: &str, turn_id: i64) -> Result<Vec<StoredEvent>>;
    fn head_seq(&self, stream: &str) -> Result<i64>;
    fn all_streams(&self) -> Result<Vec<String>>;
    /// The SQL half of [`EventStore::aggregate_streams`]: every `(n, correlation_id)` in `n` order.
    /// The cross-stream fold that drops correlated children stays backend-neutral on [`EventStore`].
    fn streams_with_correlation(&self) -> Result<Vec<(i64, Option<String>)>>;
    /// D-75: delete every stream (registry row + events) whose `updated_at` is strictly older than
    /// `cutoff_ms`, regardless of tag; returns the count. The whole-store sibling of
    /// [`prune_inactive`](Self::prune_inactive) (same delete shape, minus the `agent_id` predicate).
    /// Covers ONLY registry-listed sessions (`s_<n>` — it enumerates `streams`); ad-hoc streams are
    /// reached by [`prune_adhoc_older_than`](Self::prune_adhoc_older_than).
    fn prune_older_than(&self, cutoff_ms: i64) -> Result<usize>;
    /// D-77: delete all events of every **ad-hoc** stream — one with no `streams` registry row —
    /// whose NEWEST event `ts` is strictly older than `cutoff_ms`; returns the count of streams
    /// removed. The horizon is per-stream, not per-event (`HAVING MAX(ts) < cutoff`), so a
    /// still-active ad-hoc stream keeps its FULL history. The ad-hoc complement of
    /// [`prune_older_than`](Self::prune_older_than), which covers only registry-listed sessions.
    ///
    /// C-231: every implementation must filter its candidates through
    /// [`is_retained_from_adhoc_prune`](crate::retention::is_retained_from_adhoc_prune) — a family
    /// marked `Retained` in [`ADHOC_STREAM_FAMILIES`](crate::retention::ADHOC_STREAM_FAMILIES) is
    /// never deleted, at any age.
    fn prune_adhoc_older_than(&self, cutoff_ms: i64) -> Result<usize>;
    /// Atomic counterpart to minting a destination session via `create_session_with_context` and
    /// then repeatedly appending events one at a time (D-185): creates the session and appends
    /// every event in `events` inside ONE transaction, so a mid-copy failure (an unmappable
    /// `turn_id`, or any I/O error) leaves NOTHING listed in the registry — no half-copied session
    /// for a retry to trip over. `info` supplies the copied session's model/context and its
    /// ORIGINAL `created_at_ms`, which stamps both the destination's own `SessionStarted` and its
    /// `streams` row — so a copied fixture's registry timestamps stay consistent with its (older)
    /// events instead of showing the moment of the copy (`created_at` would otherwise be "now"
    /// while `updated_at` trails behind at the events' true, older timestamps).
    fn copy_session_atomic(&self, info: &SessionInfo, events: Vec<CopyEvent>) -> Result<String>;

    /// C-126: bound `events.db-wal` growth on a long-lived process. A default no-op — only the
    /// SQLite backend has a WAL sidecar to checkpoint; Postgres has no file-level equivalent, so
    /// it inherits this arm unchanged rather than needing its own empty override.
    fn checkpoint(&self) -> Result<()> {
        Ok(())
    }
}

/// The concrete storage backend behind an [`EventStore`]. The default build carries the embedded
/// SQLite arm and the driver-free one; a Postgres arm plugs in behind the `postgres` feature (D-73).
// `large_enum_variant` fires where the 264-byte `Sqlite` arm sits next to a much smaller one (the
// 8-byte `Postgres` arm under `--features postgres`, and `Ephemeral` since C-274). Boxing to close
// that gap is the wrong trade here: this enum is constructed once when the store opens and
// thereafter only ever touched through `&self`, so the size never costs a move — while boxing would
// put an allocation and a pointer hop on every operation of the DEFAULT backend.
#[allow(clippy::large_enum_variant)]
enum Backend {
    #[cfg(feature = "sqlite")]
    Sqlite(SqliteEvents),
    /// The driver-free, process-lifetime backend (C-274) — the only arm a `--no-default-features`
    /// build has, and what [`EventStore::ephemeral`] always builds.
    Ephemeral(EphemeralEvents),
    #[cfg(feature = "postgres")]
    Postgres(postgres::PgEvents),
}

/// The append-only event store — one ordered log projected into conversations, run traces, and turn
/// telemetry. Backed by [`SqliteEvents`] (WAL) by default; the public API is backend-independent.
pub struct EventStore {
    backend: Backend,
}

impl EventStore {
    /// Dispatch to the active backend's storage primitives.
    fn backend(&self) -> &dyn EventBackend {
        match &self.backend {
            #[cfg(feature = "sqlite")]
            Backend::Sqlite(s) => s,
            Backend::Ephemeral(e) => e,
            #[cfg(feature = "postgres")]
            Backend::Postgres(p) => p,
        }
    }

    /// Open (creating if needed) a SQLite store at `path`, with WAL enabled for concurrent reads.
    ///
    /// Needs the `sqlite` feature (on by default): this constructor promises a durable file, and the
    /// driver-free backend cannot keep that promise — silently handing back a store that forgets
    /// everything on exit would be worse than not compiling (C-274).
    #[cfg(feature = "sqlite")]
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            backend: Backend::Sqlite(SqliteEvents::open(path)?),
        })
    }

    /// An in-memory store (for tests and the SDK's ephemeral sessions) — in-memory **SQLite** when
    /// the `sqlite` feature is on (the default, and unchanged behaviour), the driver-free
    /// [`EventStore::ephemeral`] backend when it is off. Present in every feature combination on
    /// purpose: dozens of consumers construct throwaway stores with it, and none of them cares which
    /// implementation forgets the data.
    pub fn in_memory() -> Result<Self> {
        #[cfg(feature = "sqlite")]
        return Ok(Self {
            backend: Backend::Sqlite(SqliteEvents::in_memory()?),
        });
        #[cfg(not(feature = "sqlite"))]
        return Ok(Self::ephemeral());
    }

    /// A driver-free, process-lifetime store: no database driver, no file, nothing to configure —
    /// the backend a `wasm32` build has (C-274). Available in every feature combination so an
    /// embedder can ask for it by name instead of inferring it from the feature set.
    ///
    /// It is a full [`EventBackend`], held to the same conformance suite as the SQLite and Postgres
    /// backends; what it gives up is durability, so anything that must survive the process needs the
    /// SQLite (`open`) or Postgres (`open_postgres`) backend.
    pub fn ephemeral() -> Self {
        Self {
            backend: Backend::Ephemeral(EphemeralEvents::new()),
        }
    }

    /// Open a store backed by a shared Postgres (D-73), reusing an already-built [`flux_pg::PgHandle`]
    /// (pool + sync↔async bridge). Creates the schema inline on first use (`CREATE TABLE IF NOT
    /// EXISTS …`), then returns a store whose public API is byte-identical to the SQLite backend —
    /// with the added property that appends serialize across processes/replicas (a per-stream
    /// `pg_advisory_xact_lock`), which an embedded file structurally cannot do. The `postgres` feature
    /// gates this; the default build stays rusqlite-only and DB-free.
    #[cfg(feature = "postgres")]
    pub fn open_postgres(handle: std::sync::Arc<flux_pg::PgHandle>) -> Result<Self> {
        Ok(Self {
            backend: Backend::Postgres(postgres::PgEvents::connect(handle)?),
        })
    }

    // --- streams (sessions) -------------------------------------------------

    /// Mint a new session and return its id (`"s_<n>"`). Atomically registers the stream
    /// and appends its `SessionStarted` event at `stream_seq` 0. Single-tenant: the run is
    /// tagged with an empty [`EventContext`] (see [`create_session_with_context`] to tag it).
    ///
    /// [`create_session_with_context`]: Self::create_session_with_context
    pub fn create_session(&self, model: &str) -> Result<String> {
        self.create_session_with_context(model, &EventContext::default())
    }

    /// Mint a new session tagged with `ctx` (account / agent id+version / correlation id) and
    /// return its id. The context is fixed for the run's whole lifetime, recorded on the stream
    /// registry; reads of this stream's events, [`info`](Self::info), and [`list`](Self::list)
    /// carry it back, and [`list_for_account`](Self::list_for_account) scopes to it. An empty
    /// `ctx` is exactly equivalent to [`create_session`](Self::create_session).
    pub fn create_session_with_context(&self, model: &str, ctx: &EventContext) -> Result<String> {
        self.backend().create_session_with_context(model, ctx)
    }

    /// The most recently created session id, if any (for `--continue`).
    pub fn latest_session(&self) -> Result<Option<String>> {
        self.backend().latest_session()
    }

    /// Session metadata, from the registry.
    pub fn info(&self, stream: &str) -> Result<SessionInfo> {
        self.backend().info(stream)
    }

    /// The most recent sessions (newest-active first), with current message counts.
    pub fn list(&self, limit: usize) -> Result<Vec<SessionSummary>> {
        self.backend().list(limit)
    }

    /// The most recent runs for `account` (newest-active first) — the account-scoped sibling of
    /// [`list`](Self::list). Returns **only** streams tagged with that account, so a downstream
    /// multi-tenant service can enumerate one tenant's runs without seeing any other's.
    pub fn list_for_account(&self, account: &str, limit: usize) -> Result<Vec<SessionSummary>> {
        self.backend().list_for_account(account, limit)
    }

    /// The stream ids for `account`, newest-active first — the enumeration primitive a downstream
    /// consumer folds over (`account_streams` → per-stream [`conversation`](Self::conversation) /
    /// [`turns`](Self::turns)) to replay a tenant's transcripts as projections over the log.
    pub fn account_streams(&self, account: &str) -> Result<Vec<String>> {
        self.backend().account_streams(account)
    }

    /// Session search (C-164): narrow [`list`](Self::list) by free-text content, a touched-file
    /// path, and/or a date range. A **projection over the existing event log** — built entirely
    /// from [`list`](Self::list), [`conversation`](Self::conversation), and
    /// [`observations`](Self::observations), the same backend-neutral primitives every other read
    /// uses — so SQLite and Postgres return identical results for identical stores with no new
    /// SQL and no new persisted index. Results stay newest-first, capped at `limit`.
    ///
    /// An empty `filter` is byte-identical to `list(limit)` (the fast path below): a bare
    /// `flux sessions` must behave exactly as it did before this filter existed.
    ///
    /// This is the ONE seam a ranked/fuzzy matcher (C-153's TUI session picker) should plug into
    /// later — it should call `search` (or a ranking variant built the same way) instead of
    /// re-deriving its own matching over `list`.
    ///
    /// Scans every registered session (cheapest predicates — the date range, already on the
    /// summary — first; `file`/`query` only load a session's observations/conversation when
    /// their predicate is actually set). No measured need justifies a persisted index yet; if
    /// store size ever makes this scan too slow, that is the trigger to add one, not a reason to
    /// pre-build one now.
    pub fn search(&self, filter: &SessionFilter, limit: usize) -> Result<Vec<SessionSummary>> {
        if filter.is_empty() {
            return self.list(limit);
        }
        // A LIMIT large enough that no real store hits it, so this stays the exact same
        // `list` query both backends already serve identically — no new enumeration primitive.
        const UNBOUNDED: usize = i64::MAX as usize;
        let mut out = Vec::new();
        for s in self.list(UNBOUNDED)? {
            if let Some(since) = filter.since_ms {
                if s.updated_at_ms < since {
                    continue;
                }
            }
            if let Some(until) = filter.until_ms {
                if s.created_at_ms > until {
                    continue;
                }
            }
            if let Some(path) = &filter.file {
                if !self.session_touched_file(&s.id, path)? {
                    continue;
                }
            }
            if let Some(query) = &filter.query {
                if !self.session_matches_query(&s.id, query)? {
                    continue;
                }
            }
            out.push(s);
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }

    /// Whether `stream`'s conversation contains `query` (case-insensitive substring) — the `search`
    /// free-text predicate.
    fn session_matches_query(&self, stream: &str, query: &str) -> Result<bool> {
        let needle = query.to_lowercase();
        Ok(self
            .conversation(stream)?
            .iter()
            .any(|m| m.text().to_lowercase().contains(&needle)))
    }

    /// Whether `stream` recorded a `tool_call` observation whose `subjects` include `path` — the
    /// `search` file predicate. Reads the SAME durable, redacted `tool_call` observations
    /// `flux-flow` already persists (subjects are scrubbed by the live turn's `Redactor` before
    /// they ever reach the store, C-22), so a filesystem subject that happened to look
    /// secret-shaped is never exposed here — it was never stored unredacted in the first place.
    fn session_touched_file(&self, stream: &str, path: &str) -> Result<bool> {
        Ok(self.observations(stream)?.iter().any(|o| {
            o.kind == "tool_call"
                && o.data
                    .get("subjects")
                    .and_then(|v| v.as_array())
                    .is_some_and(|subjects| {
                        subjects
                            .iter()
                            .filter_map(|s| s.as_str())
                            .any(|s| path_matches(s, path))
                    })
        }))
    }

    /// A-48: the most recent stream tagged `agent_id` whose `correlation_id` equals
    /// `correlation_id` — the stateful-A2A lookup (one session per `contextId`): the server's
    /// reuse-or-mint keys on this. Newest-first so a client re-using a `contextId` after its old
    /// session was TTL-pruned (and a new one minted) always continues the LIVE session.
    pub fn find_correlated(&self, correlation_id: &str, agent_id: &str) -> Result<Option<String>> {
        self.backend().find_correlated(correlation_id, agent_id)
    }

    /// D-69: the realm-scoped sibling of [`find_correlated`](Self::find_correlated) — the most
    /// recent stream tagged `agent_id` whose `correlation_id` matches AND whose `account` equals
    /// `realm`. In a per-request-principal deployment a correlation id (an A2A `contextId`) is a
    /// logical grouping mechanism, **not** a security boundary, so continuity must be keyed within
    /// the caller's realm: two realms presenting the same correlation id continue two different
    /// sessions.
    ///
    /// `realm` is deliberately non-optional (`&str`, not `Option`). In principal mode every caller
    /// has a realm — the account when present, else a principal-derived key — so there is no
    /// legitimate `None` caller, and an `Option`-taking variant could only pick between two silent
    /// failure modes: SQL `account = ?` never matches NULL, so a `None` realm would never find its
    /// own prior session (continuity silently broken); matching NULL with `IS` instead would
    /// collapse every account-less caller into one shared NULL realm (cross-caller session
    /// leakage). Requiring the key makes both states unrepresentable. The same `=`-vs-NULL
    /// semantics is a feature here: legacy untagged streams (`account` NULL) are *structurally*
    /// unreachable through this method — they stay reachable via
    /// [`find_correlated`](Self::find_correlated), which non-principal server modes keep using
    /// unchanged.
    ///
    /// Newest-first for the same reason as [`find_correlated`](Self::find_correlated): a client
    /// re-using a correlation id after its old session was TTL-pruned always continues the LIVE
    /// session. The `account = ?3` equality is served by `idx_streams_account` (D-02), whose
    /// entries within one account come back in `n` order — so the lookup is a backward index scan
    /// bounded by the realm's own sessions, never a table scan.
    pub fn find_correlated_in_realm(
        &self,
        correlation_id: &str,
        agent_id: &str,
        realm: &str,
    ) -> Result<Option<String>> {
        self.backend()
            .find_correlated_in_realm(correlation_id, agent_id, realm)
    }

    /// A-45/C-44: the sub-agent children of `stream` — every stream whose `correlation_id` points
    /// at it (the A-08 spawn linkage: `agent_id = "subagent:<role>"`, `correlation_id` = the
    /// parent session). Oldest-first, so a replay recurses children in spawn order. One level per
    /// call; a grandchild's `correlation_id` points at ITS parent, so tree walks recurse.
    pub fn children_of(&self, stream: &str) -> Result<Vec<String>> {
        self.backend().children_of(stream)
    }

    /// Switch the session's model (records a `ModelChanged` event; the registry follows).
    pub fn set_model(&self, stream: &str, model: &str) -> Result<()> {
        self.append(
            stream,
            NewEvent::new(EventKind::ModelChanged {
                model: model.to_string(),
            }),
        )?;
        Ok(())
    }

    /// Delete sessions that recorded no messages (abandoned / test-run streams), along with
    /// their events. Returns the number of sessions removed. An empty stream has no history
    /// worth preserving, so real deletion is append-only-safe.
    pub fn prune_empty(&self) -> Result<usize> {
        self.backend().prune_empty()
    }

    /// [`prune_empty`](Self::prune_empty) with a keep-list. A listed session is never removed even
    /// when it has no messages, which lets interactive surfaces prune abandoned sessions without
    /// invalidating the active fresh session.
    pub fn prune_empty_excluding(&self, keep: &[String]) -> Result<usize> {
        self.backend().prune_empty_excluding(keep)
    }

    /// Delete sessions tagged with `agent_id` whose last activity (`updated_at`) is strictly
    /// older than `cutoff_ms`, along with their whole event streams. Returns the number of
    /// sessions removed. This is the TTL retention primitive for machine-minted surface sessions
    /// (C-18: the A2A surface tags each task session `agent_id = "a2a"` and sweeps by tag + age).
    ///
    /// Design notes:
    /// - **Real `DELETE`, not a tombstone.** The store is append-only *within* a stream — no event
    ///   is ever rewritten — but removing a *whole expired stream* is a retention decision, not a
    ///   history rewrite (the same reasoning as [`prune_empty`](Self::prune_empty)). A tombstone
    ///   would keep the rows forever and force every all-streams projection (`cost_summary_all`,
    ///   `efficiency_all`, `list`) to learn a skip rule; deleting the stream keeps them consistent
    ///   by construction — they enumerate `streams`, so a removed session simply contributes
    ///   nothing — and actually reclaims the space a TTL exists to bound. The trade-off: a pruned
    ///   session's token spend leaves the aggregate rollups, so a deployment that must retain
    ///   spend indefinitely should disable pruning (TTL 0) or snapshot its usage upstream first.
    /// - **Tag-scoped.** Only streams whose registry `agent_id` equals `agent_id` are eligible;
    ///   an untagged (CLI/TUI) session is untouchable here regardless of age.
    /// - **Age = last activity.** `updated_at` advances on every append, so a recently-active
    ///   stream survives even when it was created long before the cutoff.
    pub fn prune_inactive(&self, agent_id: &str, cutoff_ms: i64) -> Result<usize> {
        self.backend().prune_inactive(agent_id, cutoff_ms)
    }

    /// [`prune_inactive`](Self::prune_inactive) with a keep-list: a stream named in `keep` is never
    /// pruned even when its `updated_at` is past the cutoff. The A2A surface passes its in-process
    /// live-task ids (A-54): a non-blocking task queued behind the single-turn gate has a frozen
    /// `updated_at`, so without the exclusion a long queue could look expired to a *concurrent*
    /// request's mint-time sweep and be pruned out from under the queue — the C-29 hazard, re-opened
    /// by minting before the gate. Ids that don't parse as `s_<n>` are ignored (nothing to protect).
    pub fn prune_inactive_excluding(
        &self,
        agent_id: &str,
        cutoff_ms: i64,
        keep: &[String],
    ) -> Result<usize> {
        self.backend()
            .prune_inactive_excluding(agent_id, cutoff_ms, keep)
    }

    /// D-75: delete **every** stream (registry row + its whole event stream) whose last activity
    /// (`updated_at`) is strictly older than `cutoff_ms`, regardless of tag, and return the number
    /// of sessions removed. The whole-store retention primitive for long-running server deployments:
    /// scheduled "retain N days" against an unbounded log needs one call that expresses it, and
    /// [`prune_inactive`](Self::prune_inactive) structurally cannot — it is tag-scoped (matches one
    /// `agent_id`), so a deployment whose streams carry many agent tags could never sweep them all.
    ///
    /// The same delete shape as [`prune_inactive`](Self::prune_inactive) minus the tag predicate, and
    /// the same append-only reasoning: removing a *whole expired stream* is a retention decision, not
    /// a history rewrite (no event is ever mutated), and the all-streams projections stay consistent
    /// by construction because they enumerate `streams`. The same TTL/rollup trade-off applies — a
    /// pruned session's spend leaves the aggregate rollups, so a deployment that must retain spend
    /// indefinitely should snapshot usage upstream before pruning.
    ///
    /// Unlike the interactive CLI's TTL sweep, there is **no "protect the current session" carve-out**
    /// here: this is the primitive a caller that owns its store schedules against its own retention
    /// policy, so "old" means exactly `updated_at < cutoff_ms` with nothing exempt. A caller that
    /// wants to keep a live session simply keeps its `cutoff_ms` behind that session's last activity.
    ///
    /// **Scope: registry-listed sessions ONLY.** Like every registry-enumerating prune, this walks
    /// `SELECT n FROM streams …` and deletes `s_<n>` streams — an ad-hoc stream (any id that never
    /// got a registry row, e.g. a D-55 Custom-facts log) is structurally unreachable here and is
    /// reached by [`prune_adhoc_older_than`](Self::prune_adhoc_older_than) instead; a whole-store
    /// retention policy schedules both.
    pub fn prune_older_than(&self, cutoff_ms: i64) -> Result<usize> {
        self.backend().prune_older_than(cutoff_ms)
    }

    /// D-77: delete all events of every **ad-hoc** stream — one with no `streams` registry row
    /// (any stream id that doesn't name a registered `s_<n>` session, e.g. a per-tenant D-55
    /// Custom-facts log) — whose NEWEST event `ts` is strictly older than `cutoff_ms`, and return
    /// the number of streams removed (matching the other prunes' return semantics).
    ///
    /// **The horizon is per-stream, not per-event.** A stream qualifies only when its newest event
    /// predates the cutoff (`HAVING MAX(ts) < cutoff_ms`), and then its WHOLE event history is
    /// deleted; a still-active ad-hoc stream — one appended to inside the horizon — keeps its FULL
    /// history, old events included. That mirrors [`prune_older_than`](Self::prune_older_than)'s
    /// last-activity semantics (`updated_at` advances on every append) at the only granularity an
    /// unregistered stream has: its own events' timestamps.
    ///
    /// The contrast with [`prune_older_than`](Self::prune_older_than): that primitive covers ONLY
    /// registry-listed sessions (it enumerates `streams` and deletes registry row + events); this
    /// one covers ONLY unregistered streams (there is no registry row to age-check or delete — the
    /// events themselves are the whole footprint). Together they make a scheduled retention horizon
    /// actually cover the whole store.
    ///
    /// **Not every unregistered stream is disposable (C-231).** "No registry row" is a structural
    /// fact, not a statement that the data is transient: A-107's cross-session memory lives on
    /// `memory:<scope-key>` streams, which have exactly this shape, and sweeping them would delete
    /// the agent's evidence-pinned knowledge with no error and nothing left to reconstruct it from.
    /// So a family listed as `Retained` in
    /// [`ADHOC_STREAM_FAMILIES`](crate::retention::ADHOC_STREAM_FAMILIES) is skipped here at **any**
    /// age, and the returned count reflects only what was actually removed. A caller scheduling this
    /// against its own retention policy should read `retention`'s module docs: memory is deliberately
    /// exempt, and removing a memory entry is `forget_memory`'s job, not a horizon's.
    pub fn prune_adhoc_older_than(&self, cutoff_ms: i64) -> Result<usize> {
        self.backend().prune_adhoc_older_than(cutoff_ms)
    }

    /// C-126: bound `events.db-wal` growth on a long-lived process (the SQLite backend's WAL
    /// sidecar; Postgres has no file-level equivalent and no-ops). Under WAL mode, checkpointing
    /// back into the main database file needs a moment with no pinned readers; a long-lived
    /// `flux app run --serve` daemon that always holds read snapshots can defer checkpoints
    /// indefinitely (design doc `docs/designs/event-store-concurrent-use.md` §4.3), so the serve
    /// loop calls this on an idle/periodic cadence rather than relying on SQLite's own
    /// autocheckpoint (which only fires opportunistically, at a write commit).
    ///
    /// **Never blocks or errors a concurrent writer/reader** — this is a `TRUNCATE` checkpoint
    /// (R6: it competes for the write lock like a prune, so schedule off-peak) attempted on a
    /// dedicated connection with a zero busy-timeout: contention is reported back immediately
    /// rather than waited out, and is treated as "nothing to reclaim this tick" — `Ok(())`, never
    /// a turn-visible failure. Call again on the next tick; nothing is lost by skipping one.
    pub fn checkpoint(&self) -> Result<()> {
        self.backend().checkpoint()
    }

    // --- append -------------------------------------------------------------

    /// Append one event, assigning its `stream_seq` / `global_seq` / `ts` and updating the
    /// session registry — all in one transaction, so the read model never drifts from the
    /// log. If the event carries a caller-supplied `id` that already exists, this is a no-op
    /// returning the prior event (idempotent retry).
    pub fn append(&self, stream: &str, ev: NewEvent) -> Result<StoredEvent> {
        self.backend().append(stream, ev)
    }

    /// Append `ev` only if the stream's newest message-affecting event is still `expected_seq`
    /// (`-1` = none). `Ok(None)` means a concurrent writer moved the conversation on and
    /// **nothing** was appended — see [`EventBackend::append_if_conversation_head`].
    ///
    /// Deliberately crate-private: the only intended caller is [`SessionLog`](crate::SessionLog),
    /// which pairs it with a freshly-derived tail. A raw compare-and-append exposed publicly would
    /// be a second, unguarded way to write the conversation — exactly what A-100 exists to close.
    pub(crate) fn append_if_conversation_head(
        &self,
        stream: &str,
        ev: NewEvent,
        expected_seq: i64,
    ) -> Result<Option<StoredEvent>> {
        self.backend()
            .append_if_conversation_head(stream, ev, expected_seq)
    }

    /// Append several events to a stream atomically (all-or-nothing, consecutive seqs).
    pub fn append_batch(&self, stream: &str, evs: Vec<NewEvent>) -> Result<Vec<StoredEvent>> {
        let mut out = Vec::with_capacity(evs.len());
        for ev in evs {
            out.push(self.append(stream, ev)?);
        }
        Ok(out)
    }

    /// Copy one session's full event log, in order, into a fresh session on `dst` — a **different**
    /// [`EventStore`] (a different file or an in-memory scratch), not a copy within the same store.
    /// The event-export primitive behind the Test Kit's `Scenario::record` (D-174): a recorded run
    /// exports into a portable fixture directory, redacted-by-construction, safe to `git commit`.
    ///
    /// The destination session (its `SessionStarted`, `streams` row, and every other copied event)
    /// is minted in ONE transaction via the crate-private
    /// [`EventBackend::copy_session_atomic`] (D-185), so a mid-copy failure never leaves a
    /// half-copied session listed in `dst`'s registry — nothing to notice or clean up on a retry.
    /// The source's own `SessionStarted` (always `stream_seq` 0) is skipped — the destination mints
    /// its own — and every other event re-appends in its **original order**, keeping its original
    /// `ts_ms` so a re-imported run still projects the same `turns()` timeline instead of collapsing
    /// to "now". The destination's `streams.created_at`/`updated_at` are likewise stamped from the
    /// source session's own timestamps, not the moment of the copy.
    ///
    /// A `turn_id` (the `global_seq` of that turn's own `TurnStarted`) is backend-assigned and
    /// differs between the source and destination stores, so every turn-scoped event's `turn_id` is
    /// rewritten through a map built as `TurnStarted` rows are copied. A turn-scoped event whose
    /// `turn_id` cannot be resolved (a malformed or truncated source log) is a loud error — never
    /// silently dropped or left dangling on a turn that doesn't exist in `dst`.
    ///
    /// Returns the new session's id.
    ///
    /// D-185: the actual mint-and-append work happens in ONE transaction, per backend, via
    /// [`EventBackend::copy_session_atomic`] — this wrapper only reads the source (never fails
    /// partway through a write) and hands the whole event list to the destination backend, so a
    /// mid-copy failure never leaves a half-copied session listed in `dst`'s registry.
    pub fn copy_session_to(&self, session: &str, dst: &EventStore) -> Result<String> {
        let info = self.info(session)?;
        let events: Vec<CopyEvent> = self
            .load_stream(session, None)?
            .into_iter()
            .filter(|ev| !matches!(ev.kind, EventKind::SessionStarted { .. }))
            .map(|ev| CopyEvent {
                kind: ev.kind,
                schema_version: ev.schema_version,
                ts_ms: ev.ts_ms,
                turn_id: ev.turn_id,
                old_global_seq: ev.global_seq,
                id: ev.id,
            })
            .collect();
        dst.backend().copy_session_atomic(&info, events)
    }

    // --- load ---------------------------------------------------------------

    /// All events of a stream in order; `after_seq` enables incremental replay.
    pub fn load_stream(&self, stream: &str, after_seq: Option<i64>) -> Result<Vec<StoredEvent>> {
        self.backend().load_stream(stream, after_seq)
    }

    /// Events of a stream filtered by `kind` tag (e.g. `"message"`, `"run"`), in order.
    pub fn load_by_kind(&self, stream: &str, kind: &str) -> Result<Vec<StoredEvent>> {
        self.backend().load_by_kind(stream, kind)
    }

    /// The message-affecting events (`message`/`compacted`) of a stream with `stream_seq > after_seq`,
    /// in order — the incremental input for maintaining a cached conversation without re-reading and
    /// re-decoding the whole event log every planner round. `after_seq = -1` yields the full history.
    /// The kind filter is served by `idx_events_stream_kind`, so this skips the bulky plan/run/usage
    /// payloads the conversation projection would otherwise decode and discard.
    pub fn conversation_delta(&self, stream: &str, after_seq: i64) -> Result<Vec<StoredEvent>> {
        self.backend().conversation_delta(stream, after_seq)
    }

    /// Every event tagged with `turn_id`, plus its `TurnStarted` anchor (whose `global_seq`
    /// *is* the turn id), in order — the old `turn_log` + `plan_attempts` join.
    pub fn load_turn(&self, stream: &str, turn_id: i64) -> Result<Vec<StoredEvent>> {
        self.backend().load_turn(stream, turn_id)
    }

    /// The current head sequence of a stream (`-1` if empty) — the optimistic-concurrency anchor.
    pub fn head_seq(&self, stream: &str) -> Result<i64> {
        self.backend().head_seq(stream)
    }

    // --- ergonomic event-native helpers (used at call sites) ----------------

    // Conversation writes deliberately have no helper here (A-102). `record_message` /
    // `record_compaction` used to sit in this list and appended any `Message` unexamined, which is
    // how the session-shape invariant broke three times. Every conversation write now goes through
    // [`crate::SessionLog`], whose transitions cannot express an invalid shape.

    /// Record a flow run-trace event.
    pub fn record_run_event(&self, stream: &str, ev: &RunEvent) -> Result<()> {
        self.append(stream, NewEvent::run(ev.clone()))?;
        Ok(())
    }

    /// Begin a turn and return its `turn_id` (the `TurnStarted` event's `global_seq`). Use
    /// `.unwrap_or(-1)` at call sites to stay non-fatal — telemetry must never block a turn.
    pub fn begin_turn(&self, stream: &str, user_input: &str, model: &str) -> Result<i64> {
        let stored = self.append(
            stream,
            NewEvent::new(EventKind::TurnStarted {
                user_input: user_input.to_string(),
                model: model.to_string(),
            }),
        )?;
        Ok(stored.global_seq)
    }

    /// Record one planning attempt within `turn_id`. A negative `turn_id` (failed
    /// `begin_turn`) is silently skipped. Takes the same [`projection::PlanAttempt`] shape the
    /// `turns()` fold reads back, so the write and read models can't drift (C-14).
    pub fn record_plan_attempt(
        &self,
        stream: &str,
        turn_id: i64,
        attempt: projection::PlanAttempt,
    ) -> Result<()> {
        if turn_id < 0 {
            return Ok(());
        }
        self.append(
            stream,
            NewEvent::new(EventKind::PlanAttempted {
                step: attempt.step,
                outcome: attempt.outcome,
                error: attempt.error,
                fingerprint: attempt.fingerprint,
                plan_text: attempt.plan_text,
                phase: attempt.phase,
                plan_source: attempt.plan_source,
                delta_source: attempt.delta_source,
            })
            .in_turn(turn_id),
        )?;
        Ok(())
    }

    /// Persist one evidence observation (C-14). Scoped to `turn_id` when non-negative; recorded
    /// unscoped otherwise (a failed `begin_turn` must not lose the trail). Callers treat this as
    /// non-fatal (`let _ = …`) — audit writes never break a turn.
    pub fn record_observation(
        &self,
        stream: &str,
        turn_id: i64,
        obs: &flux_evidence::Observation,
    ) -> Result<()> {
        let mut ev = NewEvent::new(EventKind::Observation(obs.clone()));
        if turn_id >= 0 {
            ev = ev.in_turn(turn_id);
        }
        self.append(stream, ev)?;
        Ok(())
    }

    /// The durable evidence trail for `stream` — see [`projection::observations`].
    ///
    /// C-87: backed by a `kind = 'observation'` load (served by `idx_events_stream_kind`) rather
    /// than materializing + JSON-decoding the whole stream — the fold only reads `Observation`
    /// events, so the plan/run/usage/message payloads are never fetched or decoded. Byte-identical
    /// output to folding the full stream (the projection already discards every other kind).
    pub fn observations(&self, stream: &str) -> Result<Vec<flux_evidence::Observation>> {
        Ok(projection::observations(
            &self.load_by_kind(stream, "observation")?,
        ))
    }

    /// Record one provider call's usage, attributed to the `model` that was active for that call —
    /// the per-call granularity `TurnEnded.usage` (a single turn-total) can't express. A negative
    /// `turn_id` is a no-op, mirroring [`record_plan_attempt`](Self::record_plan_attempt).
    pub fn record_call_usage(
        &self,
        stream: &str,
        turn_id: i64,
        model: &str,
        usage: Usage,
    ) -> Result<()> {
        if turn_id < 0 {
            return Ok(());
        }
        self.append(
            stream,
            NewEvent::new(EventKind::CallUsage {
                model: model.to_string(),
                usage,
            })
            .in_turn(turn_id),
        )?;
        Ok(())
    }

    /// Register a future wake-up on `stream` (A-98): resume this session with `prompt` (+
    /// optional `context`, replayed back unchanged) once `fire_at_ms` elapses. Returns the
    /// wake-up's id — the store-minted id of the `WakeupScheduled` event itself, so callers never
    /// invent a second id scheme.
    pub fn schedule_wakeup(
        &self,
        stream: &str,
        fire_at_ms: i64,
        prompt: &str,
        context: Option<&str>,
    ) -> Result<String> {
        let stored = self.append(
            stream,
            NewEvent::new(EventKind::WakeupScheduled {
                fire_at_ms,
                prompt: prompt.to_string(),
                context: context.map(|s| s.to_string()),
            }),
        )?;
        Ok(stored.id)
    }

    /// Cancel a pending wake-up before it fires. Returns `false` (no-op) when `wakeup_id` is not
    /// currently pending on `stream` (already fired, already cancelled, or never existed) — a
    /// caller can treat that as "nothing to cancel" rather than a hard error.
    pub fn cancel_wakeup(&self, stream: &str, wakeup_id: &str) -> Result<bool> {
        if !self
            .pending_wakeups(stream)?
            .iter()
            .any(|w| w.wakeup_id == wakeup_id)
        {
            return Ok(false);
        }
        self.append(
            stream,
            NewEvent::new(EventKind::WakeupCancelled {
                wakeup_id: wakeup_id.to_string(),
            }),
        )?;
        Ok(true)
    }

    /// Record that a pending wake-up fired as `turn_id` — the ordinary turn
    /// [`flux_flow::wakeup::service_due_wakeups_on_open`] ran to service it (C-26: a real turn,
    /// not a telemetry-free shortcut). Scoped to `turn_id` like [`record_observation`](Self::record_observation)
    /// when non-negative.
    pub fn mark_wakeup_fired(&self, stream: &str, wakeup_id: &str, turn_id: i64) -> Result<()> {
        let mut ev = NewEvent::new(EventKind::WakeupFired {
            wakeup_id: wakeup_id.to_string(),
            turn_id,
        });
        if turn_id >= 0 {
            ev = ev.in_turn(turn_id);
        }
        self.append(stream, ev)?;
        Ok(())
    }

    // --- memory (A-107): its own stream in this same store, projected latest-state-per-id -----

    /// Record a new memory entry on `scope`'s `memory:<scope-key>` stream, returning the entry as
    /// stored.
    ///
    /// The entry's `id` **is** the appended `memory.noted` event's own id — one identity, minted
    /// once, so nothing has to reconcile a separate memory-id scheme against the log (the A-98
    /// wake-up precedent). `learned_at_ms` is stamped here; the citation comes from `note`, which
    /// can only be built through [`MemoryNote::new`] and is therefore already scrubbed.
    pub fn remember(&self, scope: &MemoryScope, note: MemoryNote) -> Result<MemoryEntry> {
        let id = ulid::Ulid::generate().to_string();
        let entry = MemoryEntry {
            id: id.clone(),
            claim: note.claim().to_string(),
            scope: scope.clone(),
            receipt: note.receipt,
            git: note.git,
            learned_at_ms: now_ms(),
        };
        self.append_memory(scope, memory::MEMORY_NOTED, &entry, Some(id))?;
        Ok(entry)
    }

    /// Re-state an existing entry: **appends** a `memory.edited` event carrying the whole new state
    /// for `id` — it never rewrites the original. The projection then reports the new claim while
    /// `load_stream` still yields every state the entry ever held, which is the entire point of
    /// putting memory in the log instead of a mutable side table.
    ///
    /// Returns `Ok(None)` when `id` is not currently believed in this scope (never noted, or
    /// already forgotten), mirroring [`cancel_wakeup`](Self::cancel_wakeup)'s "nothing to do" shape
    /// rather than erroring.
    pub fn amend_memory(
        &self,
        scope: &MemoryScope,
        id: &str,
        note: MemoryNote,
    ) -> Result<Option<MemoryEntry>> {
        if !self.memories(scope)?.iter().any(|m| m.id == id) {
            return Ok(None);
        }
        let entry = MemoryEntry {
            id: id.to_string(),
            claim: note.claim().to_string(),
            scope: scope.clone(),
            receipt: note.receipt,
            git: note.git,
            learned_at_ms: now_ms(),
        };
        self.append_memory(scope, memory::MEMORY_EDITED, &entry, None)?;
        Ok(Some(entry))
    }

    /// Stop believing `id`: appends a `memory.forgotten` tombstone. The entry leaves the
    /// projection; every state it held stays in the log. Returns `false` (no append) when `id` is
    /// not currently believed in this scope.
    pub fn forget_memory(&self, scope: &MemoryScope, id: &str) -> Result<bool> {
        if !self.memories(scope)?.iter().any(|m| m.id == id) {
            return Ok(false);
        }
        self.append(
            &scope.stream(),
            NewEvent::new(EventKind::Custom {
                name: memory::MEMORY_FORGOTTEN.to_string(),
                payload: serde_json::to_value(memory::MemoryTombstone { id: id.to_string() })?,
            }),
        )?;
        Ok(true)
    }

    /// The currently-believed entries for `scope` — see [`projection::memory`].
    ///
    /// Backed by a `kind = 'custom'` load (served by `idx_events_stream_kind`) rather than the full
    /// stream, following the C-87 precedent: memory events are the only `Custom` events a memory
    /// stream can hold, and no other kind contributes to this fold.
    pub fn memories(&self, scope: &MemoryScope) -> Result<Vec<MemoryEntry>> {
        Ok(projection::memory_entries(
            &self.load_by_kind(&scope.stream(), "custom")?,
        ))
    }

    /// The full append-only history of `scope`'s memory stream, oldest first — every note, edit and
    /// tombstone, including the states the projection no longer surfaces. The audit read behind
    /// "what did the agent believe, and when did it stop?".
    pub fn memory_history(&self, scope: &MemoryScope) -> Result<Vec<StoredEvent>> {
        self.load_stream(&scope.stream(), None)
    }

    /// Follow a [`Receipt`] back to the event it cites, or `None` when that event is not in this
    /// store (a pruned stream, or a citation carried in from elsewhere).
    ///
    /// Matches on the cited event's **stable id**, which is why [`Receipt::event_id`] is that id
    /// and not a `global_seq`: rowids are re-minted by any re-import or backend migration, so a
    /// `global_seq`-keyed lookup would still *succeed* afterwards — at the wrong event. A scan of
    /// the cited stream is deliberate over a store-wide id index: `stream` is part of the citation,
    /// so the lookup stays scoped to what the receipt actually claims, and this is a `memory show`
    /// path, not a per-turn one.
    pub fn resolve_receipt(&self, receipt: &Receipt) -> Result<Option<StoredEvent>> {
        Ok(self
            .load_stream(&receipt.stream, None)?
            .into_iter()
            .find(|e| e.id == receipt.event_id))
    }

    /// Append one memory event, encoding `entry` as the payload of an [`EventKind::Custom`] under
    /// `name`. The single write point for the memory stream, so the encoding cannot drift between
    /// note and edit.
    fn append_memory(
        &self,
        scope: &MemoryScope,
        name: &str,
        entry: &MemoryEntry,
        event_id: Option<String>,
    ) -> Result<StoredEvent> {
        let mut ev = NewEvent::new(EventKind::Custom {
            name: name.to_string(),
            payload: serde_json::to_value(entry)?,
        });
        if let Some(id) = event_id {
            ev = ev.with_id(id);
        }
        self.append(&scope.stream(), ev)
    }

    /// Close a turn with its final outcome, iteration count, assistant answer, and token `usage`
    /// tally (`None` when the provider reported none). A negative `turn_id` is a no-op.
    pub fn end_turn(
        &self,
        stream: &str,
        turn_id: i64,
        outcome: &str,
        iterations: u32,
        answer: &str,
        usage: Option<Usage>,
    ) -> Result<()> {
        if turn_id < 0 {
            return Ok(());
        }
        self.append(
            stream,
            NewEvent::new(EventKind::TurnEnded {
                outcome: outcome.to_string(),
                iterations,
                answer: answer.to_string(),
                usage,
            })
            .in_turn(turn_id),
        )?;
        Ok(())
    }

    // --- projections (load + fold) ------------------------------------------

    /// The conversation for a session (replaces `SessionStore::load_messages`).
    ///
    /// C-87: backed by the `kind IN ('message','compacted')` load (`conversation_delta` with
    /// `after_seq = -1` yields the full message/compacted history, served by
    /// `idx_events_stream_kind`) rather than materializing + JSON-decoding the whole stream — the
    /// conversation fold only ever reads those two kinds, so the bulky plan/run/usage payloads are
    /// never fetched or decoded. Byte-identical to folding the full stream.
    pub fn conversation(&self, stream: &str) -> Result<Vec<Message>> {
        Ok(projection::conversation(
            &self.conversation_delta(stream, -1)?,
        ))
    }

    /// The flow run-trace for a session (replaces `FlowStore::events`).
    pub fn run_trace(&self, stream: &str) -> Result<Vec<RunEvent>> {
        Ok(projection::run_trace(&self.load_by_kind(stream, "run")?))
    }

    /// The turn telemetry for a session (replaces `turn_log` + `plan_attempts`).
    pub fn turns(&self, stream: &str) -> Result<Vec<projection::TurnSummary>> {
        Ok(projection::turns(&self.load_stream(stream, None)?))
    }

    /// The wake-ups currently pending on `stream` — registered but not yet fired or cancelled
    /// (A-98). Loads only the three `wakeup_*` kinds (mirrors `observations`'s `load_by_kind`
    /// efficiency note), never the bulky message/run/usage payloads.
    pub fn pending_wakeups(&self, stream: &str) -> Result<Vec<projection::PendingWakeup>> {
        let mut events = self.load_by_kind(stream, "wakeup_scheduled")?;
        events.extend(self.load_by_kind(stream, "wakeup_fired")?);
        events.extend(self.load_by_kind(stream, "wakeup_cancelled")?);
        events.sort_by_key(|e| e.global_seq);
        Ok(projection::pending_wakeups(&events))
    }

    /// The pending wake-ups on `stream` whose `fire_at_ms` is at or before `now_ms`, oldest-due
    /// first (A-98) — what a servicing step (on-open catch-up, or a live host) should fire.
    pub fn due_wakeups(&self, stream: &str, now_ms: i64) -> Result<Vec<projection::PendingWakeup>> {
        let mut due: Vec<_> = self
            .pending_wakeups(stream)?
            .into_iter()
            .filter(|w| w.fire_at_ms <= now_ms)
            .collect();
        due.sort_by_key(|w| w.fire_at_ms);
        Ok(due)
    }

    /// Token spend + cost for one session, rolled up by model (see [`projection::cost_summary`]).
    pub fn cost_summary(
        &self,
        stream: &str,
        pricing: &flux_core::PricingTable,
    ) -> Result<Vec<projection::ModelCost>> {
        Ok(projection::cost_summary(
            &self.load_stream(stream, None)?,
            pricing,
        ))
    }

    /// Token spend + cost across every session, rolled up by model. Folds each stream's events
    /// through the SAME [`projection::cost_summary`] logic, then re-sums the per-model rows across
    /// streams — so a model that appears in several sessions reports one aggregate row. Re-merging
    /// via [`projection::RowAcc`] (C-34) — rather than re-summing raw usage and re-pricing the
    /// aggregate — keeps a model's reported-vs-table cost honest across streams too: naively
    /// re-deriving a fresh `Usage` from token totals would drop each stream's already-resolved
    /// per-call `reported_cost_usd` entirely and silently fall back to table pricing (or `None` for
    /// an untabled model) for the whole cross-stream row.
    pub fn cost_summary_all(
        &self,
        pricing: &flux_core::PricingTable,
    ) -> Result<Vec<projection::ModelCost>> {
        let streams = self.aggregate_streams()?;
        let mut per_model: std::collections::BTreeMap<String, projection::RowAcc> =
            std::collections::BTreeMap::new();
        for stream in &streams {
            for row in self.cost_summary(stream, pricing)? {
                per_model
                    .entry(row.model.clone())
                    .or_default()
                    .record_priced_row(&row);
            }
        }
        // Re-merge after the cross-stream fold (C-15): one stream may carry only the legacy bare
        // key while another carries the canonical prefixed one — the per-stream merge can't see
        // across streams, so without this the all-sessions report still splits one backend.
        Ok(projection::merge_legacy_keys(per_model)
            .into_iter()
            .map(|(model, acc)| projection::finalize_row(model, acc))
            .collect())
    }

    /// [`cost_summary_all`](Self::cost_summary_all) scoped to ONE account's streams (D-69: the
    /// realm-scoped `/usage` a multi-tenant server serves). Same fold — per-stream
    /// [`cost_summary`](Self::cost_summary) rows folded through `RowAcc` and re-merged across the
    /// legacy/canonical model-key split — so a tenant's realm-scoped total is priced and de-split
    /// **identically** to the unscoped rollup; the only difference is the stream set. Sub-agent
    /// child streams are `account = NULL` (they inherit no envelope account), so scoping to an
    /// account already excludes them, matching `cost_summary_all`'s C-23 child exclusion.
    pub fn cost_summary_for_account(
        &self,
        account: &str,
        pricing: &flux_core::PricingTable,
    ) -> Result<Vec<projection::ModelCost>> {
        let mut per_model: std::collections::BTreeMap<String, projection::RowAcc> =
            std::collections::BTreeMap::new();
        for stream in self.account_streams(account)? {
            for row in self.cost_summary(&stream, pricing)? {
                per_model
                    .entry(row.model.clone())
                    .or_default()
                    .record_priced_row(&row);
            }
        }
        Ok(projection::merge_legacy_keys(per_model)
            .into_iter()
            .map(|(model, acc)| projection::finalize_row(model, acc))
            .collect())
    }

    /// Turn-efficiency rollup for one stream — see [`projection::efficiency_summary`] (C-15).
    pub fn efficiency(&self, stream: &str) -> Result<Option<projection::EfficiencySummary>> {
        Ok(projection::efficiency_summary(
            &self.load_stream(stream, None)?,
        ))
    }

    /// Turn-efficiency rollup across every session — per-stream summaries merged by raw sums.
    pub fn efficiency_all(&self) -> Result<Option<projection::EfficiencySummary>> {
        let mut acc: Option<projection::EfficiencySummary> = None;
        for stream in self.aggregate_streams()? {
            if let Some(s) = self.efficiency(&stream)? {
                match &mut acc {
                    Some(a) => a.merge(&s),
                    None => acc = Some(s),
                }
            }
        }
        Ok(acc)
    }

    /// The session ids the all-sessions rollups ([`cost_summary_all`](Self::cost_summary_all),
    /// [`efficiency_all`](Self::efficiency_all)) fold over — every stream EXCEPT a correlated
    /// sub-agent child (C-23). A `task` sub-agent runs a full turn on its own child stream in the
    /// shared audit store, tagged with `correlation_id = <parent session>` (A-08); the parent turn
    /// *also* records that child's total as a synthetic `CallUsage` on the parent stream (the C-06
    /// rollup). Folding the child stream too would count the same spend twice, so the parent-side
    /// rollup is chosen as the single authoritative source and the correlated child is dropped from
    /// the aggregate. A stream whose `correlation_id` points OUTSIDE the current set (an orphaned or
    /// pruned parent) is kept — better to count it once than lose it. Per-stream reads
    /// ([`cost_summary`](Self::cost_summary) / [`efficiency`](Self::efficiency)) are unaffected: a
    /// child's own session still reports its full spend.
    fn aggregate_streams(&self) -> Result<Vec<String>> {
        let rows = self.backend().streams_with_correlation()?;
        let ids: std::collections::HashSet<String> =
            rows.iter().map(|(n, _)| format!("s_{n}")).collect();
        Ok(rows
            .into_iter()
            .filter(|(_, corr)| match corr {
                // Correlated child of a stream we are already folding → its spend is on the parent.
                Some(parent) => !ids.contains(parent),
                None => true,
            })
            .map(|(n, _)| format!("s_{n}"))
            .collect())
    }

    /// Every session id (`s_<n>`), oldest first — the unfiltered enumeration primitive. Unlike
    /// [`list`](Self::list) (which truncates to a limit and orders by recency for display), this
    /// returns everything. (The all-sessions rollups fold over [`aggregate_streams`](Self::aggregate_streams)
    /// instead, which excludes correlated sub-agent children to avoid double-counting — C-23.)
    pub fn all_streams(&self) -> Result<Vec<String>> {
        self.backend().all_streams()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shape::{AssistantMessage, ValidHistory};
    use crate::SessionLog;
    use flux_core::Message;

    /// A whole turn through the typed seam. Since A-102 this is the only way a conversation message
    /// reaches a log — the store carries no unguarded `record_message` for a fixture to reach for
    /// either, so these fixtures write exactly the shapes production can.
    fn say(store: &EventStore, stream: &str, user: &str, assistant: &str) {
        let mut log = SessionLog::open(store, stream).unwrap();
        log.open_turn(Message::user_text(user)).unwrap();
        log.close_turn(AssistantMessage::text(assistant).unwrap())
            .unwrap();
    }

    /// Open a turn — the `user` half alone, for the many fixtures that only need a session to be
    /// non-empty.
    fn ask(store: &EventStore, stream: &str, user: impl Into<String>) {
        SessionLog::open(store, stream)
            .unwrap()
            .open_turn(Message::user_text(user))
            .unwrap();
    }

    /// C-81: the single serde-decode point must survive an undecodable row — an unknown event
    /// variant written by a newer build, or a corrupt payload — by skipping it, not aborting the
    /// whole batch. This is what keeps every read method (`conversation`/`turns`/`observations`/
    /// `load_stream`) working across a rolling upgrade or a single corrupt byte.
    #[test]
    fn decode_all_skips_undecodable_rows_instead_of_aborting_the_read() {
        let ctx = EventContext::default();
        let good_user =
            serde_json::to_string(&EventKind::Message(Message::user_text("hello"))).unwrap();
        let good_assistant =
            serde_json::to_string(&EventKind::Message(Message::assistant_text("hi"))).unwrap();
        let raw: Vec<RawEvent> = vec![
            (1, 0, "e1".into(), 1, 1000, good_user, None),
            // A row written by a NEWER build: a `kind` tag this (closed) enum has no arm for.
            (
                2,
                1,
                "e2".into(),
                1,
                1001,
                r#"{"kind":"from_the_future","data":{"x":1}}"#.into(),
                None,
            ),
            // A genuinely corrupt payload (a known-shaped row whose JSON got mangled on disk).
            (3, 2, "e3".into(), 1, 1002, "{ not valid json".into(), None),
            (4, 3, "e4".into(), 1, 1003, good_assistant, None),
        ];
        let decoded =
            decode_all("s_1", &ctx, raw).expect("one bad row must not abort the whole read");
        // Only the two decodable rows survive; the unknown-variant and corrupt rows are skipped.
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].global_seq, 1);
        assert_eq!(decoded[1].global_seq, 4);
        // The conversation still projects from the survivors.
        assert_eq!(
            projection::conversation(&decoded)
                .iter()
                .map(|m| m.text())
                .collect::<Vec<_>>(),
            vec!["hello", "hi"]
        );
    }

    // --- conformance: ported from flux-session's test module, adapted to the event API ---

    fn create_append_load_roundtrip(store: &EventStore) {
        let id = store.create_session("claude-sonnet-4-6").unwrap();
        assert!(id.starts_with("s_"));

        say(store, &id, "hello", "hi there");

        let msgs = store.conversation(&id).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].text(), "hello");
        assert_eq!(msgs[1].text(), "hi there");
        assert_eq!(store.info(&id).unwrap().model, "claude-sonnet-4-6");
    }

    fn updated_at_advances_on_append(store: &EventStore) {
        let id = store.create_session("m").unwrap();
        let created = store.info(&id).unwrap().updated_at_ms;
        std::thread::sleep(std::time::Duration::from_millis(2));
        ask(store, &id, "hi");
        let after = store.info(&id).unwrap().updated_at_ms;
        assert!(after >= created, "updated_at must not go backwards");
        assert_eq!(store.list(1).unwrap()[0].updated_at_ms, after);
    }

    fn updated_at_advances_on_set_model(store: &EventStore) {
        let id = store.create_session("sonnet").unwrap();
        let before = store.info(&id).unwrap().updated_at_ms;
        std::thread::sleep(std::time::Duration::from_millis(2));
        store.set_model(&id, "opus").unwrap();
        let after = store.info(&id).unwrap().updated_at_ms;
        assert!(after >= before);
        assert_eq!(store.info(&id).unwrap().model, "opus");
    }

    /// The incremental `conversation_delta` fold (as the loop host maintains it) must equal a fresh
    /// full `conversation()` replay at every step — after plain appends AND across a compaction
    /// (which resets the fold). This is the correctness contract behind the loop host's cached
    /// conversation replacing the per-round full re-read.
    fn conversation_delta_folds_incrementally_like_a_full_replay(store: &EventStore) {
        let sid = store.create_session("m").unwrap();
        let mut msgs: Vec<Message> = Vec::new();
        let mut cursor = -1i64;
        let fold = |store: &EventStore, msgs: &mut Vec<Message>, cursor: &mut i64| {
            for e in store.conversation_delta(&sid, *cursor).unwrap() {
                match &e.kind {
                    EventKind::Message(m) => msgs.push(m.clone()),
                    EventKind::Compacted { messages } => {
                        msgs.clear();
                        msgs.extend(messages.iter().cloned());
                    }
                    _ => {}
                }
                *cursor = (*cursor).max(e.stream_seq);
            }
        };

        say(store, &sid, "u1", "a1");
        fold(store, &mut msgs, &mut cursor);
        assert_eq!(
            msgs,
            store.conversation(&sid).unwrap(),
            "after first appends"
        );

        // A second batch is picked up incrementally (only the new events are read).
        say(store, &sid, "u2", "a2");
        fold(store, &mut msgs, &mut cursor);
        assert_eq!(
            msgs,
            store.conversation(&sid).unwrap(),
            "after second appends"
        );
        assert_eq!(msgs.len(), 4);

        // A compaction in the delta resets the fold to the snapshot.
        let snapshot = vec![
            Message::user_text("[summary]"),
            Message::assistant_text("a2"),
        ];
        SessionLog::open(store, &sid)
            .unwrap()
            .rewrite(ValidHistory::new(snapshot.clone()).unwrap())
            .unwrap();
        fold(store, &mut msgs, &mut cursor);
        assert_eq!(msgs, store.conversation(&sid).unwrap(), "after compaction");
        assert_eq!(msgs, snapshot);

        // A message after the compaction appends onto the snapshot, not the pre-compaction history.
        ask(store, &sid, "u3");
        fold(store, &mut msgs, &mut cursor);
        assert_eq!(
            msgs,
            store.conversation(&sid).unwrap(),
            "after post-compaction append"
        );
        assert_eq!(msgs.len(), 3);
    }

    fn compaction_replaces_the_live_view_but_keeps_history(store: &EventStore) {
        let id = store.create_session("m").unwrap();
        let mut log = SessionLog::open(store, &id).unwrap();
        for i in 0..5 {
            if i % 2 == 0 {
                log.open_turn(Message::user_text(format!("m{i}"))).unwrap();
            } else {
                log.close_turn(AssistantMessage::text(format!("m{i}")).unwrap())
                    .unwrap();
            }
        }
        assert_eq!(store.conversation(&id).unwrap().len(), 5);

        log.rewrite(
            ValidHistory::new(vec![
                Message::user_text("summary"),
                Message::assistant_text("recent"),
            ])
            .unwrap(),
        )
        .unwrap();
        let msgs = store.conversation(&id).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].text(), "summary");
        assert_eq!(msgs[1].text(), "recent");

        // appending after a compaction continues from the snapshot
        log.open_turn(Message::user_text("more")).unwrap();
        assert_eq!(store.conversation(&id).unwrap().len(), 3);

        // history is retained: the 5 superseded Message events are still on disk
        let raw = store.load_stream(&id, None).unwrap();
        let messages = raw
            .iter()
            .filter(|e| e.kind.kind_tag() == "message")
            .count();
        let compactions = raw
            .iter()
            .filter(|e| e.kind.kind_tag() == "compacted")
            .count();
        assert_eq!(messages, 6, "5 pre-compaction + 1 post-compaction");
        assert_eq!(compactions, 1);

        // the list count tracks the live conversation length, not the raw event count
        assert_eq!(store.list(1).unwrap()[0].messages, 3);
        assert_eq!(
            store.list(1).unwrap()[0].messages,
            store.conversation(&id).unwrap().len()
        );
    }

    fn latest_session_tracks_newest(store: &EventStore) {
        assert!(store.latest_session().unwrap().is_none());
        let _a = store.create_session("m").unwrap();
        let b = store.create_session("m").unwrap();
        assert_eq!(store.latest_session().unwrap(), Some(b));
    }

    fn unknown_session_has_no_conversation_but_info_errors(store: &EventStore) {
        // The log accepts any stream; an unknown one simply has no events.
        assert!(store.conversation("s_999").unwrap().is_empty());
        assert!(store.conversation("nope").unwrap().is_empty());
        // The registry, however, has no row for it.
        assert!(store.info("s_999").is_err());
    }

    fn list_returns_newest_first_with_counts(store: &EventStore) {
        let a = store.create_session("m1").unwrap();
        say(store, &a, "hi", "there");
        let b = store.create_session("m2").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        ask(store, &a, "last");

        let list = store.list(10).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, a, "most recently active first");
        assert_eq!(list[0].messages, 3);
        assert_eq!(list[1].id, b);
        assert_eq!(list[1].messages, 0);
        assert_eq!(list[0].model, "m1");
        assert_eq!(store.list(1).unwrap().len(), 1);
    }

    fn set_model_updates_listing(store: &EventStore) {
        let a = store.create_session("sonnet").unwrap();
        store.set_model(&a, "opus").unwrap();
        assert_eq!(store.list(1).unwrap()[0].model, "opus");
        assert_eq!(store.info(&a).unwrap().model, "opus");
    }

    /// C-164: `search`'s three predicates (free-text query, touched-file, date range) each select
    /// only the session(s) they should and exclude the others, and results stay newest-first —
    /// seeded over a multi-session store exactly like a real `flux sessions --query|--file|--since`
    /// invocation would see.
    fn search_selects_matching_sessions_and_excludes_others(store: &EventStore) {
        use flux_evidence::{Observation, Phase};

        // Session A (oldest): mentions "pandas", touched src/animals.rs.
        let a = store.create_session("m").unwrap();
        ask(store, &a, "let's talk about pandas today");
        store
            .record_observation(
                &a,
                -1,
                &Observation::new(
                    "tool_call",
                    Phase::Turn,
                    serde_json::json!({
                        "tool": "read",
                        "subjects": ["src/animals.rs"],
                        "caller": "user",
                    }),
                ),
            )
            .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(3));
        let mid_cutoff = now_ms(); // separates A's activity from B/C's
        std::thread::sleep(std::time::Duration::from_millis(3));

        // Session B: mentions "flask", touched src/web/routes.py.
        let b = store.create_session("m").unwrap();
        ask(store, &b, "deploying the flask app");
        store
            .record_observation(
                &b,
                -1,
                &Observation::new(
                    "tool_call",
                    Phase::Turn,
                    serde_json::json!({
                        "tool": "write",
                        "subjects": ["src/web/routes.py"],
                        "caller": "user",
                    }),
                ),
            )
            .unwrap();

        // Session C (newest): no matching content or file — proves exclusion.
        let c = store.create_session("m").unwrap();
        ask(store, &c, "unrelated chit chat");

        // --- query: case-insensitive, selects only the matching session ---
        let by_query = store
            .search(&SessionFilter::new().with_query("PANDAS"), 10)
            .unwrap();
        assert_eq!(
            by_query.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
            vec![a.clone()],
            "query matches only the session whose content contains it"
        );

        // --- file: path-boundary match, selects only the session that touched it ---
        let by_file = store
            .search(&SessionFilter::new().with_file("routes.py"), 10)
            .unwrap();
        assert_eq!(
            by_file.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
            vec![b.clone()],
            "file filter matches only the session that touched that path"
        );
        // A near-miss suffix (missing the path boundary) must not match — this is a
        // path-boundary test, not a raw substring test.
        assert!(
            store
                .search(&SessionFilter::new().with_file("outes.py"), 10)
                .unwrap()
                .is_empty(),
            "a non-path-boundary suffix must not match"
        );

        // --- date range: selects only sessions active at/after the cutoff (B, C), excludes A ---
        let by_date = store
            .search(&SessionFilter::new().with_since_ms(mid_cutoff), 10)
            .unwrap();
        let date_ids: Vec<_> = by_date.iter().map(|s| s.id.clone()).collect();
        assert!(date_ids.contains(&b), "B is active after the cutoff");
        assert!(date_ids.contains(&c), "C is active after the cutoff");
        assert!(!date_ids.contains(&a), "A's activity predates the cutoff");

        // --- newest-first holds under actual filtering (not just the `list` fast path) ---
        let all = store
            .search(&SessionFilter::new().with_since_ms(0), 10)
            .unwrap();
        assert_eq!(
            all.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
            vec![c.clone(), b.clone(), a.clone()],
            "results stay newest-first"
        );
    }

    /// C-164 redaction: `flux-flow`'s `flush_observations` scrubs every observation through the
    /// live turn's `Redactor` BEFORE it ever reaches the store (C-22) — this test seeds a session
    /// the same way, then proves the second Acceptance case explicitly: a secret's own plaintext
    /// must never be usable as a search term that confirms the redacted session's existence, and
    /// the content `search` reads back never carries the plaintext either.
    fn search_query_cannot_recover_a_redacted_secret(store: &EventStore) {
        // Deliberately NOT a credential-shaped prefix (`sk-`, `ghp_`, …) — this must be redacted
        // because it was REGISTERED, proving the registered-value path specifically, not the
        // separate always-on credential-shape heuristic.
        let redactor = flux_secret::Redactor::new();
        let secret = "orchid-galaxy-turnip-9317";
        redactor.add_secret(secret.to_string());

        let id = store.create_session("m").unwrap();
        // Exactly the C-22 seam: redact BEFORE the store ever sees the payload.
        let raw = format!("the deploy passphrase is: {secret}");
        ask(store, &id, redactor.redact(&raw));

        // The secret's own plaintext must not confirm the session's existence.
        assert!(
            store
                .search(&SessionFilter::new().with_query(secret), 10)
                .unwrap()
                .is_empty(),
            "the raw secret must not match the session that contains it (redacted)"
        );
        // The redacted marker legitimately matches — proving the query path itself works and
        // this isn't passing merely because nothing matches anything.
        let redacted_hits = store
            .search(&SessionFilter::new().with_query("[redacted]"), 10)
            .unwrap();
        assert_eq!(redacted_hits.len(), 1);
        assert_eq!(redacted_hits[0].id, id);
        // And the content search reads back never carries the plaintext.
        let stored_text = store.conversation(&id).unwrap()[0].text().to_string();
        assert!(
            !stored_text.contains(secret),
            "stored/rendered content must never reveal the secret"
        );
    }

    /// A-48: the stateful-A2A lookup — newest live stream by (correlation_id, agent_id); other
    /// agent tags and other correlation ids never match.
    fn find_correlated_returns_newest_matching_tagged_stream(store: &EventStore) {
        let mk = |agent: &str, corr: &str| {
            store
                .create_session_with_context(
                    "m",
                    &EventContext {
                        agent_id: Some(agent.into()),
                        correlation_id: Some(corr.into()),
                        ..Default::default()
                    },
                )
                .unwrap()
        };
        let _old = mk("a2a", "ctx-1");
        let newest = mk("a2a", "ctx-1");
        let _other_tag = mk("subagent:review", "ctx-1");
        let _other_ctx = mk("a2a", "ctx-2");

        assert_eq!(
            store.find_correlated("ctx-1", "a2a").unwrap().as_deref(),
            Some(newest.as_str()),
            "newest a2a stream for the contextId wins"
        );
        assert_eq!(store.find_correlated("ctx-404", "a2a").unwrap(), None);
        assert_eq!(store.find_correlated("ctx-2", "other").unwrap(), None);
    }

    /// D-69: a correlation id is a grouping mechanism, not a security boundary — the same
    /// (correlation_id, agent_id) presented under two different realms names two DIFFERENT
    /// sessions, and each realm's lookup returns only its own.
    fn find_correlated_in_realm_isolates_realms(store: &EventStore) {
        let mk = |realm: &str| {
            store
                .create_session_with_context(
                    "m",
                    &EventContext {
                        account: Some(realm.into()),
                        agent_id: Some("a2a".into()),
                        correlation_id: Some("ctx-1".into()),
                        ..Default::default()
                    },
                )
                .unwrap()
        };
        let acme = mk("acme");
        let globex = mk("globex");
        assert_ne!(acme, globex, "one session per (realm, contextId)");

        assert_eq!(
            store
                .find_correlated_in_realm("ctx-1", "a2a", "acme")
                .unwrap()
                .as_deref(),
            Some(acme.as_str())
        );
        assert_eq!(
            store
                .find_correlated_in_realm("ctx-1", "a2a", "globex")
                .unwrap()
                .as_deref(),
            Some(globex.as_str())
        );
        // A realm with no session for the contextId sees nothing — not another realm's session.
        assert_eq!(
            store
                .find_correlated_in_realm("ctx-1", "a2a", "initech")
                .unwrap(),
            None
        );
    }

    /// D-69: `account = ?` never matches SQL NULL, so a legacy untagged session is structurally
    /// unreachable through the realm-scoped lookup for EVERY realm — while the untouched
    /// [`EventStore::find_correlated`] still finds it (non-principal server modes keep today's
    /// behavior byte-for-byte).
    fn find_correlated_in_realm_never_matches_legacy_untagged_sessions(store: &EventStore) {
        let legacy = store
            .create_session_with_context(
                "m",
                &EventContext {
                    agent_id: Some("a2a".into()),
                    correlation_id: Some("ctx-old".into()),
                    ..Default::default() // account: None — a pre-principal-mode session
                },
            )
            .unwrap();

        for realm in ["acme", "globex", ""] {
            assert_eq!(
                store
                    .find_correlated_in_realm("ctx-old", "a2a", realm)
                    .unwrap(),
                None,
                "NULL-account session must not surface in realm {realm:?}"
            );
        }
        assert_eq!(
            store.find_correlated("ctx-old", "a2a").unwrap().as_deref(),
            Some(legacy.as_str()),
            "the unscoped lookup still serves non-principal modes"
        );
    }

    /// D-69: within one realm the newest matching stream wins, mirroring `find_correlated`'s
    /// TTL-prune reasoning — a re-used contextId always continues the LIVE session.
    fn find_correlated_in_realm_returns_newest_within_the_realm(store: &EventStore) {
        let ctx = EventContext {
            account: Some("acme".into()),
            agent_id: Some("a2a".into()),
            correlation_id: Some("ctx-1".into()),
            ..Default::default()
        };
        let _old = store.create_session_with_context("m", &ctx).unwrap();
        let newest = store.create_session_with_context("m", &ctx).unwrap();

        assert_eq!(
            store
                .find_correlated_in_realm("ctx-1", "a2a", "acme")
                .unwrap()
                .as_deref(),
            Some(newest.as_str()),
            "newest stream in the realm wins"
        );
    }

    fn record_call_usage_is_turn_scoped_and_a_noop_on_negative_turn_id(store: &EventStore) {
        let a = store.create_session("m").unwrap();
        let turn_id = store.begin_turn(&a, "hi", "m").unwrap();
        store
            .record_call_usage(
                &a,
                turn_id,
                "m",
                Usage {
                    input_tokens: 10,
                    output_tokens: 5,
                    ..Default::default()
                },
            )
            .unwrap();
        // A no-op on a negative turn_id (mirrors record_plan_attempt/end_turn) — never fatal.
        store
            .record_call_usage(&a, -1, "m", Usage::default())
            .unwrap();

        let events = store.load_stream(&a, None).unwrap();
        let calls: Vec<_> = events
            .iter()
            .filter(|e| matches!(e.kind, EventKind::CallUsage { .. }))
            .collect();
        assert_eq!(calls.len(), 1, "the negative-turn_id call recorded nothing");
    }

    fn cost_summary_wraps_the_projection_over_one_stream(store: &EventStore) {
        let a = store.create_session("claude-sonnet-4-6").unwrap();
        let turn_id = store.begin_turn(&a, "hi", "claude-sonnet-4-6").unwrap();
        store
            .record_call_usage(
                &a,
                turn_id,
                "claude-sonnet-4-6",
                Usage {
                    input_tokens: 1_000_000,
                    output_tokens: 1_000_000,
                    ..Default::default()
                },
            )
            .unwrap();
        store
            .end_turn(&a, turn_id, "accepted", 1, "done", None)
            .unwrap();

        let pricing = flux_core::PricingTable::builtin();
        let summary = store.cost_summary(&a, &pricing).unwrap();
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].model, "claude-sonnet-4-6");
        assert_eq!(summary[0].usage.input_tokens, 1_000_000);
        // 1·3 + 1·15 = 18
        assert!((summary[0].cost.unwrap().usd - 18.0).abs() < 1e-9);
    }

    fn cost_summary_all_aggregates_across_sessions(store: &EventStore) {
        let a = store.create_session("claude-sonnet-4-6").unwrap();
        let ta = store.begin_turn(&a, "hi", "claude-sonnet-4-6").unwrap();
        store
            .record_call_usage(
                &a,
                ta,
                "claude-sonnet-4-6",
                Usage {
                    input_tokens: 1_000_000,
                    ..Default::default()
                },
            )
            .unwrap();

        let b = store.create_session("claude-sonnet-4-6").unwrap();
        let tb = store.begin_turn(&b, "hi", "claude-sonnet-4-6").unwrap();
        store
            .record_call_usage(
                &b,
                tb,
                "claude-sonnet-4-6",
                Usage {
                    input_tokens: 500_000,
                    ..Default::default()
                },
            )
            .unwrap();

        assert_eq!(store.all_streams().unwrap(), vec![a.clone(), b.clone()]);

        let pricing = flux_core::PricingTable::builtin();
        let summary = store.cost_summary_all(&pricing).unwrap();
        assert_eq!(summary.len(), 1, "one model across both sessions");
        assert_eq!(
            summary[0].usage.input_tokens, 1_500_000,
            "the two sessions' spend is summed, not just the last one's"
        );
    }

    fn cost_summary_for_account_scopes_and_sums_through_the_shared_fold(store: &EventStore) {
        let acct = |a: &str| EventContext {
            account: Some(a.to_string()),
            ..Default::default()
        };
        let record = |ctx: &EventContext, tokens: u64| {
            let s = store
                .create_session_with_context("claude-sonnet-4-6", ctx)
                .unwrap();
            let t = store.begin_turn(&s, "hi", "claude-sonnet-4-6").unwrap();
            store
                .record_call_usage(
                    &s,
                    t,
                    "claude-sonnet-4-6",
                    Usage {
                        input_tokens: tokens,
                        ..Default::default()
                    },
                )
                .unwrap();
        };
        // Two acme streams + one globex stream.
        record(&acct("acme"), 1_000_000);
        record(&acct("acme"), 500_000);
        record(&acct("globex"), 9_000_000);

        let pricing = flux_core::PricingTable::builtin();
        let acme = store.cost_summary_for_account("acme", &pricing).unwrap();
        assert_eq!(acme.len(), 1, "one model row across acme's two streams");
        assert_eq!(
            acme[0].usage.input_tokens, 1_500_000,
            "acme's two streams are summed through the same fold as cost_summary_all"
        );
        // Isolation: globex's spend is not visible under acme, and vice versa.
        let globex = store.cost_summary_for_account("globex", &pricing).unwrap();
        assert_eq!(globex[0].usage.input_tokens, 9_000_000);
        assert!(store
            .cost_summary_for_account("nobody", &pricing)
            .unwrap()
            .is_empty());
    }

    fn prune_empty_removes_zero_message_sessions(store: &EventStore) {
        let a = store.create_session("m").unwrap();
        ask(store, &a, "hi");
        let _b = store.create_session("m").unwrap();
        let _c = store.create_session("m").unwrap();

        assert_eq!(store.list(10).unwrap().len(), 3);
        let pruned = store.prune_empty().unwrap();
        assert_eq!(pruned, 2);
        let remaining = store.list(10).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, a);
        assert_eq!(store.latest_session().unwrap(), Some(a));
    }

    fn prune_empty_excluding_preserves_active_empty_sessions(store: &EventStore) {
        let active = store.create_session("active-model").unwrap();
        let abandoned = store.create_session("old-model").unwrap();
        let populated = store.create_session("kept-model").unwrap();
        ask(store, &populated, "keep me");

        assert_eq!(
            store
                .prune_empty_excluding(std::slice::from_ref(&active))
                .unwrap(),
            1
        );
        assert!(
            store.info(&active).is_ok(),
            "the active empty session survives"
        );
        assert!(
            store.info(&abandoned).is_err(),
            "other empty sessions are pruned"
        );
        assert!(store.info(&populated).is_ok(), "non-empty sessions survive");
    }

    fn prune_inactive_deletes_only_expired_streams_with_the_tag(store: &EventStore) {
        let a2a = EventContext {
            agent_id: Some("a2a".into()),
            ..Default::default()
        };

        // An a2a-tagged session with recorded spend — will expire.
        let old = store.create_session_with_context("m", &a2a).unwrap();
        let t = store.begin_turn(&old, "hi", "claude-sonnet-4-6").unwrap();
        store
            .record_call_usage(
                &old,
                t,
                "claude-sonnet-4-6",
                Usage {
                    input_tokens: 7,
                    ..Default::default()
                },
            )
            .unwrap();
        store
            .end_turn(&old, t, "accepted", 1, "done", None)
            .unwrap();
        // An untagged (CLI) session just as old — never eligible, whatever its age.
        let cli = store.create_session("m").unwrap();
        let tc = store.begin_turn(&cli, "hi", "other-model").unwrap();
        store
            .record_call_usage(
                &cli,
                tc,
                "other-model",
                Usage {
                    input_tokens: 3,
                    ..Default::default()
                },
            )
            .unwrap();
        store
            .end_turn(&cli, tc, "accepted", 1, "done", None)
            .unwrap();
        // A session tagged by a DIFFERENT agent — not eligible for the "a2a" sweep.
        let other = store
            .create_session_with_context(
                "m",
                &EventContext {
                    agent_id: Some("subagent:scout".into()),
                    ..Default::default()
                },
            )
            .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(3));
        let cutoff = now_ms(); // everything above is now strictly older than this
        std::thread::sleep(std::time::Duration::from_millis(3));
        // A fresh a2a session minted after the cutoff — tagged, but not expired.
        let fresh = store.create_session_with_context("m", &a2a).unwrap();

        assert_eq!(store.prune_inactive("a2a", cutoff).unwrap(), 1);
        assert!(store.info(&old).is_err(), "expired a2a stream is gone");
        assert!(
            store.load_stream(&old, None).unwrap().is_empty(),
            "its whole event stream is gone too (no partial streams)"
        );
        assert!(store.info(&cli).is_ok(), "untagged session survives");
        assert!(store.info(&other).is_ok(), "other agent's tag survives");
        assert!(store.info(&fresh).is_ok(), "recent a2a session survives");
        assert_eq!(store.all_streams().unwrap().len(), 3);

        // The all-streams projections stay consistent after the delete: they enumerate
        // `streams`, so the pruned session simply no longer contributes.
        let pricing = flux_core::PricingTable::builtin();
        let costs = store.cost_summary_all(&pricing).unwrap();
        assert_eq!(costs.len(), 1, "only the surviving session's model remains");
        assert_eq!(costs[0].usage.input_tokens, 3);
        let eff = store.efficiency_all().unwrap().expect("cli's turn remains");
        assert_eq!(eff.turns, 1, "the pruned session's turn no longer folds in");

        // A second sweep at the same cutoff is a no-op (idempotent).
        assert_eq!(store.prune_inactive("a2a", cutoff).unwrap(), 0);
    }

    fn prune_inactive_excluding_protects_the_keep_list(store: &EventStore) {
        let a2a = EventContext {
            agent_id: Some("a2a".into()),
            ..Default::default()
        };
        // Two a2a sessions that will both look expired…
        let queued = store.create_session_with_context("m", &a2a).unwrap();
        let stale = store.create_session_with_context("m", &a2a).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(3));
        let cutoff = now_ms();
        // …but `queued` is on the keep-list (an in-process task merely queued behind the turn
        // gate, A-54), so only `stale` is swept.
        assert_eq!(
            store
                .prune_inactive_excluding("a2a", cutoff, std::slice::from_ref(&queued))
                .unwrap(),
            1
        );
        assert!(store.info(&queued).is_ok(), "kept session survives");
        assert!(store.info(&stale).is_err(), "unprotected session is swept");
        // An empty keep-list behaves exactly like `prune_inactive` (sweeps the remainder)…
        assert_eq!(
            store.prune_inactive_excluding("a2a", cutoff, &[]).unwrap(),
            1
        );
        assert!(store.info(&queued).is_err());
        // …and a non-`s_<n>` id on the keep-list is ignored, never an error.
        assert_eq!(
            store
                .prune_inactive_excluding("a2a", cutoff, &["audit-x".into()])
                .unwrap(),
            0
        );
    }

    /// C-87: `prune_empty` must gate on `last_seq <= 0`, NOT `msg_count == 0`. A session that
    /// recorded durable facts other than `Message`s — a run trace, an observation, a `CallUsage`,
    /// an app `Custom` event — has `msg_count == 0` but real history worth keeping; only a session
    /// holding nothing but its seq-0 `SessionStarted` is truly empty.
    fn prune_empty_keeps_sessions_with_durable_nonmessage_facts(store: &EventStore) {
        // A session whose only content is one durable observation (no Message) — msg_count stays 0
        // but last_seq advances to 1, so it is NOT empty.
        let with_obs = store.create_session("m").unwrap();
        store
            .record_observation(
                &with_obs,
                -1,
                &flux_evidence::Observation::new(
                    "tool_call",
                    flux_evidence::Phase::Turn,
                    serde_json::json!({ "tool": "read" }),
                ),
            )
            .unwrap();
        // A session with a run-trace fact but no Message — likewise non-empty.
        let with_run = store.create_session("m").unwrap();
        store
            .record_run_event(
                &with_run,
                &RunEvent::StepSucceeded {
                    step: "s1".into(),
                    output: "v_1".into(),
                },
            )
            .unwrap();
        // A genuinely empty session (only its SessionStarted) — must be pruned.
        let empty = store.create_session("m").unwrap();

        assert_eq!(
            store.prune_empty().unwrap(),
            1,
            "only the truly empty session is pruned"
        );
        assert!(
            store.info(&with_obs).is_ok(),
            "a session with an observation but no message survives prune"
        );
        assert_eq!(store.observations(&with_obs).unwrap().len(), 1);
        assert!(
            store.info(&with_run).is_ok(),
            "a session with a run-trace fact but no message survives prune"
        );
        assert!(store.info(&empty).is_err(), "the empty session is pruned");
    }

    /// C-87: `observations()` is backed by a `kind = 'observation'` load, and `conversation()` by a
    /// `kind IN ('message','compacted')` load — each must still return exactly its own kind and
    /// nothing else, even when the stream interleaves messages, run events, turn telemetry, and
    /// observations. This guards the kind-filtered read against dropping needed rows or leaking
    /// others.
    fn projections_read_only_their_kinds_from_a_mixed_stream(store: &EventStore) {
        let id = store.create_session("m").unwrap();
        let turn = store.begin_turn(&id, "go", "m").unwrap();
        let mut log = SessionLog::open(store, &id).unwrap();
        log.open_turn(Message::user_text("u1")).unwrap();
        store
            .record_run_event(
                &id,
                &RunEvent::StepSucceeded {
                    step: "s1".into(),
                    output: "v_1".into(),
                },
            )
            .unwrap();
        store
            .record_observation(
                &id,
                turn,
                &flux_evidence::Observation::new(
                    "tool_call",
                    flux_evidence::Phase::Turn,
                    serde_json::json!({ "tool": "read" }),
                ),
            )
            .unwrap();
        store
            .record_call_usage(
                &id,
                turn,
                "m",
                Usage {
                    input_tokens: 5,
                    ..Default::default()
                },
            )
            .unwrap();
        log.close_turn(AssistantMessage::text("a1").unwrap())
            .unwrap();

        // conversation() sees only the two messages, in order — not the run/turn/usage/observation
        // events also on the stream.
        assert_eq!(
            store
                .conversation(&id)
                .unwrap()
                .iter()
                .map(|m| m.text())
                .collect::<Vec<_>>(),
            vec!["u1", "a1"]
        );
        // observations() sees only the one observation.
        let obs = store.observations(&id).unwrap();
        assert_eq!(obs.len(), 1);
        assert_eq!(obs[0].kind, "tool_call");
    }

    fn roles_round_trip_through_the_conversation(store: &EventStore) {
        let id = store.create_session("m").unwrap();
        say(store, &id, "q", "a");
        let roles: Vec<_> = store
            .conversation(&id)
            .unwrap()
            .iter()
            .map(|m| format!("{:?}", m.role).to_lowercase())
            .collect();
        assert_eq!(roles, vec!["user", "assistant"]);
    }

    fn append_is_transactional_and_sequences_monotonically(store: &EventStore) {
        let id = store.create_session("m").unwrap();
        // `append` is this test's subject, so it is driven directly rather than through the typed
        // session log — what is under test is the stream's sequencing, not a conversation's shape.
        for i in 0..10 {
            store
                .append(&id, NewEvent::message(Message::user_text(format!("m{i}"))))
                .unwrap();
        }
        assert_eq!(store.conversation(&id).unwrap().len(), 10);
        // SessionStarted (seq 0) + 10 messages → head seq 10, contiguous.
        assert_eq!(store.head_seq(&id).unwrap(), 10);
    }

    // --- event-store specific behavior ---

    fn run_events_and_turn_telemetry_share_the_log(store: &EventStore) {
        let id = store.create_session("m").unwrap();

        let turn = store.begin_turn(&id, "do it", "m").unwrap();
        store
            .record_plan_attempt(
                &id,
                turn,
                projection::PlanAttempt {
                    step: 0,
                    outcome: "compile_error".into(),
                    error: Some("boom".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        store
            .record_plan_attempt(
                &id,
                turn,
                projection::PlanAttempt {
                    step: 1,
                    outcome: "accepted".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        store
            .record_run_event(
                &id,
                &RunEvent::StepSucceeded {
                    step: "s1".into(),
                    output: "v_1".into(),
                },
            )
            .unwrap();
        ask(store, &id, "hi");
        store
            .end_turn(
                &id,
                turn,
                "accepted",
                2,
                "done",
                Some(Usage {
                    input_tokens: 100,
                    output_tokens: 20,
                    ..Default::default()
                }),
            )
            .unwrap();

        // run trace projection
        let trace = store.run_trace(&id).unwrap();
        assert_eq!(trace.len(), 1);
        assert!(matches!(trace[0], RunEvent::StepSucceeded { .. }));

        // turn telemetry projection
        let turns = store.turns(&id).unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].outcome, "accepted");
        assert_eq!(turns[0].iterations, 2);
        assert_eq!(turns[0].plan_attempts.len(), 2);
        // token usage survives the SQLite payload round-trip
        assert_eq!(turns[0].usage.as_ref().map(|u| u.total()), Some(120));

        // load_turn returns the anchor plus its scoped children
        let turn_events = store.load_turn(&id, turn).unwrap();
        let kinds: Vec<_> = turn_events.iter().map(|e| e.kind.kind_tag()).collect();
        assert_eq!(
            kinds,
            vec![
                "turn_started",
                "plan_attempted",
                "plan_attempted",
                "turn_ended"
            ]
        );

        // the conversation projection ignores run/turn events
        assert_eq!(store.conversation(&id).unwrap().len(), 1);
    }

    fn idempotent_append_with_a_stable_id(store: &EventStore) {
        let id = store.create_session("m").unwrap();
        let first = store
            .append(
                &id,
                NewEvent::message(Message::user_text("once")).with_id("evt-1"),
            )
            .unwrap();
        let again = store
            .append(
                &id,
                NewEvent::message(Message::user_text("once")).with_id("evt-1"),
            )
            .unwrap();
        assert_eq!(first.global_seq, again.global_seq, "retry is a no-op");
        assert_eq!(store.conversation(&id).unwrap().len(), 1);
    }

    // --- D-02: tenant/agent context envelope ---

    fn context_round_trips_on_stored_events_and_summaries(store: &EventStore) {
        let ctx = EventContext {
            account: Some("acme".into()),
            agent_id: Some("support-bot".into()),
            agent_version: Some("v3".into()),
            correlation_id: Some("corr-1".into()),
        };
        let id = store.create_session_with_context("m", &ctx).unwrap();
        ask(store, &id, "hi");

        // Every event read back from the stream carries the run context (SessionStarted + msg).
        let events = store.load_stream(&id, None).unwrap();
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|e| e.context == ctx));
        // The append return value carries it too.
        let stored = store
            .append(&id, NewEvent::message(Message::user_text("again")))
            .unwrap();
        assert_eq!(stored.context, ctx);
        // The session registry views surface it.
        assert_eq!(store.info(&id).unwrap().context, ctx);
        assert_eq!(store.list(1).unwrap()[0].context, ctx);
    }

    fn accounts_are_isolated_in_scoped_reads(store: &EventStore) {
        let a = store
            .create_session_with_context("m", &EventContext::for_account("a"))
            .unwrap();
        let b = store
            .create_session_with_context("m", &EventContext::for_account("b"))
            .unwrap();
        ask(store, &a, "ax");
        ask(store, &b, "bx");

        // list_for_account returns only that account's run.
        let a_list = store.list_for_account("a", 10).unwrap();
        assert_eq!(a_list.len(), 1);
        assert_eq!(a_list[0].id, a);
        assert_eq!(a_list[0].context.account.as_deref(), Some("a"));
        // account_streams is the same isolation, as bare ids.
        assert_eq!(store.account_streams("b").unwrap(), vec![b.clone()]);
        // An unknown account sees nothing; the global list still sees both runs.
        assert!(store.list_for_account("nope", 10).unwrap().is_empty());
        assert!(store.account_streams("nope").unwrap().is_empty());
        assert_eq!(store.list(10).unwrap().len(), 2);
    }

    fn single_tenant_session_has_empty_context(store: &EventStore) {
        let id = store.create_session("m").unwrap();
        ask(store, &id, "hi");

        // The single-tenant path is unchanged: every surface carries an empty envelope.
        assert!(store.info(&id).unwrap().context.is_empty());
        assert!(store.list(1).unwrap()[0].context.is_empty());
        assert!(store
            .load_stream(&id, None)
            .unwrap()
            .iter()
            .all(|e| e.context.is_empty()));
        // An untagged run never surfaces in account-scoped reads.
        assert!(store.list_for_account("anything", 10).unwrap().is_empty());
        assert!(store.account_streams("anything").unwrap().is_empty());
    }

    /// C-23: the `flux usage` all-sessions rollups must not double-count sub-agent spend. A sub-agent
    /// runs a full turn on its OWN correlated child stream in the shared audit store (A-08) AND its
    /// total is rolled up as a synthetic `CallUsage` on the parent turn (C-06). `cost_summary_all` /
    /// `efficiency_all` fold every stream, so unless correlated child streams are excluded the child's
    /// N tokens land twice. The parent-side rollup is the single authoritative source; the child
    /// stream's own per-session reporting stays intact.
    fn cost_summary_all_does_not_double_count_correlated_children(store: &EventStore) {
        // Parent turn: records the child's total as a synthetic `CallUsage` (the C-06 rollup).
        let parent = store.create_session("claude-sonnet-4-6").unwrap();
        let tp = store
            .begin_turn(&parent, "delegate", "claude-sonnet-4-6")
            .unwrap();
        store
            .record_call_usage(
                &parent,
                tp,
                "child-model",
                Usage {
                    input_tokens: 1_000_000,
                    ..Default::default()
                },
            )
            .unwrap();
        store
            .end_turn(&parent, tp, "accepted", 1, "done", None)
            .unwrap();

        // The child runs its own turn on a correlated child stream in the SAME store (A-08).
        let child = store
            .create_session_with_context(
                "child-model",
                &EventContext {
                    agent_id: Some("subagent:scout".into()),
                    correlation_id: Some(parent.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
        let tc = store.begin_turn(&child, "scout", "child-model").unwrap();
        store
            .record_call_usage(
                &child,
                tc,
                "child-model",
                Usage {
                    input_tokens: 1_000_000,
                    ..Default::default()
                },
            )
            .unwrap();
        store
            .end_turn(&child, tc, "accepted", 1, "done", None)
            .unwrap();

        let pricing = flux_core::PricingTable::builtin();

        // All-sessions cost: the child's 1M tokens counted ONCE (the parent-side rollup), not 2M,
        // and the synthetic parent-side call is the single call, not two.
        let all = store.cost_summary_all(&pricing).unwrap();
        let child_row = all
            .iter()
            .find(|m| m.model == "child-model")
            .expect("child-model row present");
        assert_eq!(
            child_row.usage.input_tokens, 1_000_000,
            "child spend counted once, not doubled: {all:?}"
        );
        assert_eq!(
            child_row.calls, 1,
            "the synthetic parent-side call is the single authoritative source: {all:?}"
        );

        // Per-session (single-stream) reporting is UNCHANGED: the child's own stream still reports
        // its full spend, and the parent's still includes the rolled-up child total.
        let child_solo = store.cost_summary(&child, &pricing).unwrap();
        assert_eq!(
            child_solo
                .iter()
                .find(|m| m.model == "child-model")
                .unwrap()
                .usage
                .input_tokens,
            1_000_000
        );
        let parent_solo = store.cost_summary(&parent, &pricing).unwrap();
        assert_eq!(
            parent_solo
                .iter()
                .find(|m| m.model == "child-model")
                .unwrap()
                .usage
                .input_tokens,
            1_000_000
        );

        // Efficiency all-sessions: the correlated child turn does not fold in twice either — the
        // parent turn is the top-level unit and the sub-agent's work rolls into it.
        let eff = store.efficiency_all().unwrap().expect("completed turns");
        assert_eq!(
            eff.turns, 1,
            "only the parent turn counts at top level; the sub-agent turn rolls into it"
        );
        assert_eq!(
            eff.calls, 1,
            "the single synthetic call, not the child's duplicate"
        );
    }

    /// D-55: `EventKind::Custom` rides the existing `append`/`NewEvent` path with `EventContext`
    /// account scoping exactly like every other kind — appended under an account, read back through
    /// the account-scoped path, its opaque `payload` survives byte-identical, and every other
    /// projection (conversation, msg_count) is unaffected by its presence.
    fn custom_events_append_and_read_back_scoped_by_account(store: &EventStore) {
        let ctx = EventContext::for_account("acme");
        let id = store.create_session_with_context("m", &ctx).unwrap();
        let mut log = SessionLog::open(store, &id).unwrap();
        log.open_turn(Message::user_text("hi")).unwrap();

        let payload = serde_json::json!({"tool": "read_file", "path": "src/lib.rs", "bytes": 128});
        let stored = store
            .append(
                &id,
                NewEvent::new(EventKind::Custom {
                    name: "audit.tool_call".to_string(),
                    payload: payload.clone(),
                }),
            )
            .unwrap();
        assert_eq!(
            stored.context, ctx,
            "a Custom row carries the run's account scoping like any other event"
        );

        log.close_turn(AssistantMessage::text("done").unwrap())
            .unwrap();

        // Read back through the account-scoped path (the downstream-consumer surface) and find the
        // exact row: the payload survives byte-identical, not merely structurally equal.
        assert_eq!(store.account_streams("acme").unwrap(), vec![id.clone()]);
        let events = store.load_stream(&id, None).unwrap();
        let custom = events
            .iter()
            .find(|e| matches!(&e.kind, EventKind::Custom { .. }))
            .expect("the Custom row round-trips through the store");
        match &custom.kind {
            EventKind::Custom { name, payload: p } => {
                assert_eq!(name, "audit.tool_call");
                assert_eq!(
                    p, &payload,
                    "payload survives byte-identical through the SQLite round trip"
                );
            }
            other => panic!("expected Custom, got {other:?}"),
        }
        assert_eq!(custom.context, ctx);
        assert_eq!(custom.kind.kind_tag(), "custom");

        // Other projections are unaffected: the conversation is still exactly the two messages, and
        // the Custom row didn't bump msg_count or otherwise perturb the session registry.
        assert_eq!(
            store.conversation(&id).unwrap(),
            vec![Message::user_text("hi"), Message::assistant_text("done")]
        );
        assert_eq!(
            store.list_for_account("acme", 10).unwrap()[0].messages,
            2,
            "msg_count tracks only Message/Compacted events, not Custom"
        );
    }

    /// A-98: `schedule_wakeup`/`cancel_wakeup`/`mark_wakeup_fired` fold correctly through
    /// `pending_wakeups` — scheduled minus whatever has since fired or been cancelled, oldest
    /// registered first, and every field (including `context`) round-trips intact.
    fn wakeup_schedule_cancel_and_fire_fold_into_pending_correctly(store: &EventStore) {
        let id = store.create_session("m").unwrap();

        let a = store
            .schedule_wakeup(&id, 1_000, "check the deploy", Some("deploy id: abc123"))
            .unwrap();
        let b = store
            .schedule_wakeup(&id, 2_000, "second wake", None)
            .unwrap();
        let c = store
            .schedule_wakeup(&id, 3_000, "third wake", None)
            .unwrap();
        assert_ne!(a, b);
        assert_ne!(b, c);

        let pending = store.pending_wakeups(&id).unwrap();
        assert_eq!(
            pending.len(),
            3,
            "all three are pending right after scheduling"
        );
        assert_eq!(
            pending
                .iter()
                .map(|w| w.wakeup_id.clone())
                .collect::<Vec<_>>(),
            vec![a.clone(), b.clone(), c.clone()],
            "oldest-registered first"
        );
        assert_eq!(pending[0].fire_at_ms, 1_000);
        assert_eq!(pending[0].prompt, "check the deploy");
        assert_eq!(pending[0].context.as_deref(), Some("deploy id: abc123"));
        assert!(pending[1].context.is_none());

        // Cancel the middle one: it drops out, the other two are untouched.
        assert!(store.cancel_wakeup(&id, &b).unwrap());
        let pending = store.pending_wakeups(&id).unwrap();
        assert_eq!(
            pending
                .iter()
                .map(|w| w.wakeup_id.clone())
                .collect::<Vec<_>>(),
            vec![a.clone(), c.clone()]
        );

        // Firing the first one also removes it from the pending set.
        store.mark_wakeup_fired(&id, &a, 42).unwrap();
        let pending = store.pending_wakeups(&id).unwrap();
        assert_eq!(
            pending
                .iter()
                .map(|w| w.wakeup_id.clone())
                .collect::<Vec<_>>(),
            vec![c.clone()]
        );
    }

    /// A-98: `cancel_wakeup` is a no-op (`false`, no event appended) for an id that is unknown,
    /// already fired, or already cancelled — a caller can treat that as "nothing to cancel"
    /// instead of a hard error.
    fn cancel_wakeup_is_a_noop_for_an_unknown_or_already_resolved_id(store: &EventStore) {
        let id = store.create_session("m").unwrap();
        assert!(!store.cancel_wakeup(&id, "not-a-real-id").unwrap());

        let w = store.schedule_wakeup(&id, 1_000, "wake", None).unwrap();
        assert!(
            store.cancel_wakeup(&id, &w).unwrap(),
            "first cancel succeeds"
        );
        assert!(
            !store.cancel_wakeup(&id, &w).unwrap(),
            "a second cancel of the same id is a no-op, not a double-cancel event"
        );

        let fired = store.schedule_wakeup(&id, 1_000, "wake", None).unwrap();
        store.mark_wakeup_fired(&id, &fired, 1).unwrap();
        assert!(
            !store.cancel_wakeup(&id, &fired).unwrap(),
            "an already-fired wake-up cannot be cancelled"
        );
    }

    /// A-98: `due_wakeups` returns only wake-ups whose `fire_at_ms` has elapsed, soonest-due first
    /// — the order a servicing step should fire them in.
    fn due_wakeups_filters_by_fire_at_and_sorts_soonest_first(store: &EventStore) {
        let id = store.create_session("m").unwrap();
        let soon = store.schedule_wakeup(&id, 1_000, "soon", None).unwrap();
        let later = store.schedule_wakeup(&id, 5_000, "later", None).unwrap();
        let far = store
            .schedule_wakeup(&id, 9_000, "far future", None)
            .unwrap();

        let due = store.due_wakeups(&id, 5_000).unwrap();
        assert_eq!(
            due.iter().map(|w| w.wakeup_id.clone()).collect::<Vec<_>>(),
            vec![soon.clone(), later.clone()],
            "`far` (9_000) is not yet due at now=5_000"
        );

        let due_all = store.due_wakeups(&id, 10_000).unwrap();
        assert_eq!(
            due_all
                .iter()
                .map(|w| w.wakeup_id.clone())
                .collect::<Vec<_>>(),
            vec![soon, later, far]
        );
    }

    /// A-98 acceptance: a pending wake-up is cleared when its session is deleted — with NO new
    /// code, because registrations ride the session's own event stream. Every existing whole-stream
    /// delete primitive (`prune_older_than` here; `prune_inactive`/the D-75 whole-store sweep are
    /// the same shape) already deletes the wake-up along with everything else in the stream.
    fn deleting_a_session_clears_its_pending_wakeups(store: &EventStore) {
        let id = store.create_session("m").unwrap();
        ask(store, &id, "hi");
        store
            .schedule_wakeup(&id, 9_999_999_999_999, "far future", None)
            .unwrap();
        assert_eq!(store.pending_wakeups(&id).unwrap().len(), 1);

        std::thread::sleep(std::time::Duration::from_millis(3));
        let cutoff = now_ms();
        assert_eq!(
            store.prune_older_than(cutoff).unwrap(),
            1,
            "the session is deleted"
        );

        assert!(store.info(&id).is_err(), "the session itself is gone");
        assert!(
            store.pending_wakeups(&id).unwrap().is_empty(),
            "its pending wake-up is gone too — no separate cleanup needed"
        );
    }

    /// D-75: `prune_older_than` deletes every stream older than the cutoff regardless of tag — the
    /// whole-store retention primitive. Streams straddling the cutoff: the old ones (registry row
    /// AND events) go, a fresh one survives; an empty-store call is a no-op.
    fn prune_older_than_deletes_streams_straddling_the_cutoff(store: &EventStore) {
        // Empty-store call is a no-op.
        assert_eq!(
            store.prune_older_than(now_ms()).unwrap(),
            0,
            "empty store prunes nothing"
        );

        // Two old streams with DIFFERENT tags — prune_older_than is tag-agnostic (unlike prune_inactive).
        let old = store.create_session("m").unwrap();
        ask(store, &old, "old");
        let tagged_old = store
            .create_session_with_context(
                "m",
                &EventContext {
                    agent_id: Some("a2a".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        ask(store, &tagged_old, "old2");

        std::thread::sleep(std::time::Duration::from_millis(3));
        let cutoff = now_ms(); // everything above is now strictly older than this
        std::thread::sleep(std::time::Duration::from_millis(3));

        // A fresh stream created after the cutoff — must survive.
        let fresh = store.create_session("m").unwrap();
        ask(store, &fresh, "new");

        assert_eq!(
            store.prune_older_than(cutoff).unwrap(),
            2,
            "both old streams pruned regardless of their tags"
        );
        assert!(store.info(&old).is_err(), "old registry row gone");
        assert!(
            store.load_stream(&old, None).unwrap().is_empty(),
            "old events gone too (no partial streams)"
        );
        assert!(
            store.info(&tagged_old).is_err(),
            "the tagged old stream is pruned too (whole-store, not tag-scoped)"
        );
        assert!(store.load_stream(&tagged_old, None).unwrap().is_empty());
        assert!(store.info(&fresh).is_ok(), "the fresh stream survives");
        assert_eq!(store.conversation(&fresh).unwrap().len(), 1);

        // A second sweep at the same cutoff is a no-op (idempotent).
        assert_eq!(store.prune_older_than(cutoff).unwrap(), 0);
    }

    /// D-77: `prune_adhoc_older_than` reaches the streams the registry-enumerating prunes
    /// structurally cannot — ad-hoc streams (non-`s_<n>` ids, no `streams` row) — and ONLY those.
    /// The horizon is per-stream on the NEWEST event: an aged ad-hoc stream is deleted whole, a
    /// still-active one keeps its FULL history (old events included), and registered sessions are
    /// out of scope entirely — even one older than the cutoff.
    fn prune_adhoc_older_than_reaches_only_aged_unregistered_streams(store: &EventStore) {
        let fact = |n: u32| {
            NewEvent::new(EventKind::Custom {
                name: "audit.fact".to_string(),
                payload: serde_json::json!({ "n": n }),
            })
        };

        // Before the cutoff: an ad-hoc stream that will age out entirely…
        store.append("audit-old", fact(1)).unwrap();
        store.append("audit-old", fact(2)).unwrap();
        // …an ad-hoc stream that gets an OLD event but stays active past the cutoff…
        store.append("audit-fresh", fact(3)).unwrap();
        // …and an aged REGISTERED session — registry rows are prune_older_than's territory, so
        // this primitive must leave it alone regardless of age.
        let aged_registered = store.create_session("m").unwrap();
        ask(store, &aged_registered, "old");

        std::thread::sleep(std::time::Duration::from_millis(15));
        let cutoff = now_ms(); // everything above is now strictly older than this
        std::thread::sleep(std::time::Duration::from_millis(3));

        // Inside the horizon: the second ad-hoc stream stays active, and a fresh session appears.
        store.append("audit-fresh", fact(4)).unwrap();
        let fresh_registered = store.create_session("m").unwrap();
        ask(store, &fresh_registered, "new");

        assert_eq!(
            store.prune_adhoc_older_than(cutoff).unwrap(),
            1,
            "exactly the one aged ad-hoc stream is removed"
        );
        assert!(
            store.load_stream("audit-old", None).unwrap().is_empty(),
            "the aged ad-hoc stream's events are gone"
        );
        assert_eq!(store.head_seq("audit-old").unwrap(), -1);
        // Per-stream horizon, not per-event: the active ad-hoc stream keeps its FULL history,
        // including the event older than the cutoff.
        assert_eq!(
            store.load_stream("audit-fresh", None).unwrap().len(),
            2,
            "a still-active ad-hoc stream keeps its whole history"
        );
        // Registered sessions are untouched — including the AGED one (its retention belongs to
        // prune_older_than, not this primitive).
        assert!(store.info(&aged_registered).is_ok());
        assert_eq!(store.conversation(&aged_registered).unwrap().len(), 1);
        assert!(store.info(&fresh_registered).is_ok());
        assert_eq!(store.conversation(&fresh_registered).unwrap().len(), 1);

        // A second sweep at the same cutoff is a no-op (idempotent).
        assert_eq!(store.prune_adhoc_older_than(cutoff).unwrap(), 0);
    }

    /// C-231: `memory:<scope-key>` streams (A-107) have exactly the shape
    /// `prune_adhoc_older_than` targets — no `streams` registry row — so an unguarded ad-hoc sweep
    /// would delete cross-session memory silently and unrecoverably. The prune must skip every
    /// family [`crate::retention::ADHOC_STREAM_FAMILIES`] marks `Retained`, *at any age*, while
    /// still doing its job on an ordinary aged ad-hoc stream in the same sweep.
    fn prune_adhoc_older_than_never_deletes_cross_session_memory(store: &EventStore) {
        let sid = store.create_session("m").unwrap();
        let (cited, turn_id) = cited_event(store, &sid);
        let global = MemoryScope::Global;
        let project = MemoryScope::Project {
            key: "flux".to_string(),
        };
        let g = store
            .remember(
                &global,
                note_citing("prefers terse answers", &cited, Some(turn_id)),
            )
            .unwrap();
        let p = store
            .remember(
                &project,
                note_citing(
                    "the auth middleware is in src/mw/auth.rs",
                    &cited,
                    Some(turn_id),
                ),
            )
            .unwrap();
        // An ordinary ad-hoc stream alongside them, so the sweep is proved to still work rather
        // than to have been neutered into a no-op.
        store
            .append(
                "audit-old",
                NewEvent::new(EventKind::Custom {
                    name: "audit.fact".to_string(),
                    payload: serde_json::json!({ "n": 1 }),
                }),
            )
            .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(15));
        let cutoff = now_ms(); // both memory streams are now strictly older than the horizon
        std::thread::sleep(std::time::Duration::from_millis(3));

        assert_eq!(
            store.prune_adhoc_older_than(cutoff).unwrap(),
            1,
            "the aged ordinary ad-hoc stream ages out; the aged memory streams are not candidates"
        );
        assert!(
            store.load_stream("audit-old", None).unwrap().is_empty(),
            "the ordinary ad-hoc stream still prunes — the guard is scoped, not a blanket opt-out"
        );

        // Every scope survives: the read model, and the append-only history behind it.
        assert_eq!(
            store
                .memories(&global)
                .unwrap()
                .iter()
                .map(|m| m.id.clone())
                .collect::<Vec<_>>(),
            vec![g.id.clone()],
            "global memory must survive an ad-hoc prune whose cutoff is past it"
        );
        assert_eq!(
            store
                .memories(&project)
                .unwrap()
                .iter()
                .map(|m| m.id.clone())
                .collect::<Vec<_>>(),
            vec![p.id.clone()],
            "a project scope is a second memory stream and must survive on the same grounds"
        );
        assert_eq!(store.memory_history(&global).unwrap().len(), 1);
        assert_eq!(store.memory_history(&project).unwrap().len(), 1);
        assert_eq!(store.load_stream(&global.stream(), None).unwrap().len(), 1);

        // Idempotent, and a repeat sweep still does not reach memory.
        assert_eq!(store.prune_adhoc_older_than(cutoff).unwrap(), 0);
        assert_eq!(store.memories(&global).unwrap().len(), 1);
        assert_eq!(store.memories(&project).unwrap().len(), 1);
    }

    // --- D-174: copy_session_to (event-export) ------------------------------

    /// `copy_session_to` must reproduce every projection byte-for-byte: `conversation`, `turns`
    /// (including the ORIGINAL `started_at_ms`/`ended_at_ms`, preserved by the atomic copy), `run_trace`
    /// (cell order intact), and `cost_summary`. No duplicated `SessionStarted` (the destination's own
    /// `create_session_with_context` already mints one), so the total event count matches exactly.
    #[test]
    fn copy_session_to_reproduces_every_projection() {
        let src = EventStore::in_memory().unwrap();
        let ctx = EventContext {
            account: Some("acct-1".into()),
            agent_id: Some("agent-1".into()),
            ..Default::default()
        };
        let sid = src.create_session_with_context("model-a", &ctx).unwrap();

        let t1 = src.begin_turn(&sid, "hello", "model-a").unwrap();
        let mut log = SessionLog::open(&src, &sid).unwrap();
        log.open_turn(Message::user_text("hello")).unwrap();
        src.record_run_event(
            &sid,
            &RunEvent::OpRecorded {
                seq: 0,
                step: "s1".into(),
                op: "read".into(),
                input_hash: "h1".into(),
                input_hash_redacted: None,
                input_view: None,
                input_view_truncated: false,
                content: "file contents".into(),
                view: None,
                is_error: false,
                denied: false,
                redacted: false,
                truncated: false,
            },
        )
        .unwrap();
        src.record_call_usage(
            &sid,
            t1,
            "model-a",
            Usage {
                input_tokens: 10,
                output_tokens: 5,
                ..Default::default()
            },
        )
        .unwrap();
        log.close_turn(AssistantMessage::text("hi").unwrap())
            .unwrap();
        src.end_turn(&sid, t1, "answered", 1, "hi", None).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(2));

        let t2 = src.begin_turn(&sid, "again", "model-a").unwrap();
        log.open_turn(Message::user_text("again")).unwrap();
        src.record_run_event(
            &sid,
            &RunEvent::OpRecorded {
                seq: 1,
                step: "s2".into(),
                op: "write".into(),
                input_hash: "h2".into(),
                input_hash_redacted: None,
                input_view: None,
                input_view_truncated: false,
                content: "wrote".into(),
                view: None,
                is_error: false,
                denied: false,
                redacted: false,
                truncated: false,
            },
        )
        .unwrap();
        src.record_call_usage(
            &sid,
            t2,
            "model-a",
            Usage {
                input_tokens: 20,
                output_tokens: 8,
                ..Default::default()
            },
        )
        .unwrap();
        log.close_turn(AssistantMessage::text("again reply").unwrap())
            .unwrap();
        src.end_turn(&sid, t2, "answered", 1, "again reply", None)
            .unwrap();

        let dst = EventStore::in_memory().unwrap();
        let new_id = src.copy_session_to(&sid, &dst).unwrap();

        assert_eq!(
            src.conversation(&sid).unwrap(),
            dst.conversation(&new_id).unwrap(),
            "conversation projection"
        );
        assert_eq!(
            src.run_trace(&sid).unwrap(),
            dst.run_trace(&new_id).unwrap(),
            "run_trace (cell order) projection"
        );

        let src_turns = src.turns(&sid).unwrap();
        let dst_turns = dst.turns(&new_id).unwrap();
        assert_eq!(src_turns.len(), 2);
        assert_eq!(dst_turns.len(), 2);
        for (s, d) in src_turns.iter().zip(dst_turns.iter()) {
            assert_eq!(s.user_input, d.user_input);
            assert_eq!(s.outcome, d.outcome);
            assert_eq!(s.answer, d.answer);
            assert_eq!(s.call_usage, d.call_usage);
            assert_eq!(
                s.started_at_ms, d.started_at_ms,
                "ts_ms preserved (started)"
            );
            assert_eq!(s.ended_at_ms, d.ended_at_ms, "ts_ms preserved (ended)");
        }

        let pricing = flux_core::PricingTable::builtin();
        assert_eq!(
            src.cost_summary(&sid, &pricing).unwrap(),
            dst.cost_summary(&new_id, &pricing).unwrap(),
            "cost_summary projection"
        );

        // No duplicated SessionStarted: destination's own (from create_session_with_context)
        // replaces the source's, so the total event count matches exactly.
        assert_eq!(
            src.load_stream(&sid, None).unwrap().len(),
            dst.load_stream(&new_id, None).unwrap().len(),
            "no duplicated SessionStarted"
        );

        let info = dst.info(&new_id).unwrap();
        assert_eq!(info.model, "model-a");
        assert_eq!(info.context.account.as_deref(), Some("acct-1"));
    }

    /// The destination's `TurnStarted` global_seqs are backend-assigned afresh (a fresh session in a
    /// fresh — or merely different — store), so every turn-scoped event's `turn_id` must be rewritten
    /// through the copy's own map, never left pointing at the SOURCE's turn ids.
    #[test]
    fn copy_session_to_remaps_turn_ids_to_the_destination_session() {
        let src = EventStore::in_memory().unwrap();
        let sid = src.create_session("model-a").unwrap();
        let t1 = src.begin_turn(&sid, "one", "model-a").unwrap();
        src.record_call_usage(&sid, t1, "model-a", Usage::default())
            .unwrap();
        src.end_turn(&sid, t1, "answered", 1, "ok", None).unwrap();
        let t2 = src.begin_turn(&sid, "two", "model-a").unwrap();
        src.record_call_usage(&sid, t2, "model-a", Usage::default())
            .unwrap();
        src.end_turn(&sid, t2, "answered", 1, "ok", None).unwrap();

        let dst = EventStore::in_memory().unwrap();
        let new_id = dst.create_session("occupy-earlier-ids").unwrap();
        // Give `dst` some unrelated prior history so its next-minted global_seqs are guaranteed to
        // differ from the source's — the remap must not accidentally "work" only because both
        // stores' sequences happen to line up.
        ask(&dst, &new_id, "unrelated");

        let copied_id = src.copy_session_to(&sid, &dst).unwrap();
        let dst_events = dst.load_stream(&copied_id, None).unwrap();
        let dst_turn_starts: std::collections::HashSet<i64> = dst_events
            .iter()
            .filter(|e| matches!(e.kind, EventKind::TurnStarted { .. }))
            .map(|e| e.global_seq)
            .collect();
        assert_eq!(dst_turn_starts.len(), 2);
        // Not the source's own turn ids (proves this is a real remap, not a passthrough).
        assert!(!dst_turn_starts.contains(&t1));
        assert!(!dst_turn_starts.contains(&t2));
        for e in &dst_events {
            if let Some(turn_id) = e.turn_id {
                assert!(
                    dst_turn_starts.contains(&turn_id),
                    "every scoped event's turn_id must resolve to a copied TurnStarted in dst, \
                     got {turn_id} not in {dst_turn_starts:?}"
                );
            }
        }
    }

    /// A turn-scoped event whose `turn_id` doesn't correspond to any `TurnStarted` copied so far (a
    /// malformed/partial source log) is a loud error — never silently dropped or left dangling.
    #[test]
    fn copy_session_to_errors_loudly_on_an_unmappable_turn_id() {
        let src = EventStore::in_memory().unwrap();
        let sid = src.create_session("model-a").unwrap();
        src.append(
            &sid,
            NewEvent::new(EventKind::CallUsage {
                model: "model-a".into(),
                usage: Usage::default(),
            })
            .in_turn(999_999),
        )
        .unwrap();

        let dst = EventStore::in_memory().unwrap();
        let err = src.copy_session_to(&sid, &dst).unwrap_err();
        assert!(
            err.to_string().contains("999999"),
            "expected the unmappable turn_id in the diagnostic, got: {err}"
        );

        // D-185 item 3: a copy that fails partway (the unmappable turn_id above) must leave NO
        // trace in `dst`'s registry — no half-copied session for a retry to trip over. Before the
        // atomic rewrite, `create_session_with_context` had already minted and listed the
        // destination session by the time the loop hit the unmappable turn_id.
        assert_eq!(
            dst.list(10).unwrap(),
            Vec::new(),
            "a failed copy_session_to must leave no listed session in dst"
        );
    }

    /// D-185 item 4: a copied fixture's registry timestamps must stay consistent with its (older)
    /// copied events — never `created_at > updated_at`, which is what a destination `SessionStarted`
    /// stamped `now_ms()` alongside events keeping their original, older `ts_ms` would produce.
    #[test]
    fn copy_session_to_keeps_registry_timestamps_consistent_with_copied_events() {
        let src = EventStore::in_memory().unwrap();
        let sid = src.create_session("model-a").unwrap();
        let t1 = src.begin_turn(&sid, "hi", "model-a").unwrap();
        ask(&src, &sid, "hi");
        src.end_turn(&sid, t1, "answered", 1, "hi back", None)
            .unwrap();

        // Simulate a fixture recorded well in the past: real recordings/replays span real
        // wall-clock time, so the destination `created_at` (source's original) is always <= every
        // copied event's `ts_ms`. Sleeping a couple ms is enough to prove the destination isn't
        // simply stamping "now" over old events.
        std::thread::sleep(std::time::Duration::from_millis(2));

        let dst = EventStore::in_memory().unwrap();
        let new_id = src.copy_session_to(&sid, &dst).unwrap();
        let info = dst.info(&new_id).unwrap();
        assert!(
            info.created_at_ms <= info.updated_at_ms,
            "copied session must not show created_at ({}) > updated_at ({})",
            info.created_at_ms,
            info.updated_at_ms
        );
        // And the copy must not have raced ahead to "now": both timestamps are pinned to the
        // source session's own (older) history, not the moment `copy_session_to` ran.
        let src_info = src.info(&sid).unwrap();
        assert_eq!(info.created_at_ms, src_info.created_at_ms);
    }

    // --- A-107: the memory stream and its projection --------------------------------------

    /// Seed a session with one turn and return the event a memory can honestly cite (the user
    /// message), plus the turn it belongs to — the shape A-108's host-stamped citation will
    /// produce for real.
    fn cited_event(store: &EventStore, stream: &str) -> (StoredEvent, i64) {
        let turn_id = store
            .begin_turn(stream, "where is the auth middleware?", "m")
            .unwrap();
        ask(store, stream, "where is the auth middleware?");
        let cited = store
            .load_stream(stream, None)
            .unwrap()
            .into_iter()
            .last()
            .expect("the seeded turn appended at least one event");
        (cited, turn_id)
    }

    /// A note citing `cited`, scrubbed through a real `Redactor` — the same call shape production
    /// uses, so the fixtures can only write what A-108 can write.
    fn note_citing(claim: &str, cited: &StoredEvent, turn_id: Option<i64>) -> MemoryNote {
        let redactor = flux_secret::Redactor::new();
        MemoryNote::new(
            claim,
            Receipt {
                stream: cited.stream.clone(),
                event_id: cited.id.clone(),
                turn_id,
            },
            Some(crate::GitPin {
                sha: "a1b2c3d".to_string(),
                paths: vec!["src/mw/auth.rs".to_string()],
            }),
            |s| redactor.redact(s),
        )
    }

    /// A-107 item 3: entries live on their OWN `memory:<scope-key>` stream in this same store — not
    /// on the learning session and not in a side table — and the projection is latest-state-per-id.
    /// Two scopes are two streams, and neither can see the other's entries.
    fn memory_entries_live_on_a_scoped_stream_in_the_same_store(store: &EventStore) {
        let sid = store.create_session("m").unwrap();
        let (cited, turn_id) = cited_event(store, &sid);

        let global = MemoryScope::Global;
        let project = MemoryScope::Project {
            key: "flux".to_string(),
        };
        let g = store
            .remember(
                &global,
                note_citing("prefers terse answers", &cited, Some(turn_id)),
            )
            .unwrap();
        let p = store
            .remember(
                &project,
                note_citing(
                    "the auth middleware is in src/mw/auth.rs",
                    &cited,
                    Some(turn_id),
                ),
            )
            .unwrap();

        // The entries are on the memory streams, and the learning session is untouched by them.
        assert_eq!(
            store
                .memories(&global)
                .unwrap()
                .iter()
                .map(|m| m.id.clone())
                .collect::<Vec<_>>(),
            vec![g.id.clone()],
            "the global scope must see exactly its own entry"
        );
        assert_eq!(
            store
                .memories(&project)
                .unwrap()
                .iter()
                .map(|m| m.id.clone())
                .collect::<Vec<_>>(),
            vec![p.id.clone()],
            "scopes are separate streams, so neither sees the other's entries"
        );
        assert!(
            store
                .load_stream(&sid, None)
                .unwrap()
                .iter()
                .all(|e| !matches!(&e.kind, EventKind::Custom { name, .. } if name.starts_with("memory."))),
            "memory must not be written onto the learning session's stream"
        );
        // The entry id IS its `memory.noted` event's id — one identity, no second scheme.
        let history = store.memory_history(&global).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].id, g.id);
        assert_eq!(history[0].stream, "memory:global");
        // And the scope travels in the payload, so an exported entry stays self-describing.
        assert_eq!(store.memories(&project).unwrap()[0].scope, project);
    }

    /// A-107 item 4 (the failing-first headline): an **edit appends** rather than mutates and a
    /// **forget appends a tombstone**; the projection reflects both, while the log keeps every
    /// state the entry ever held. A forgotten entry disappears from the read model *and* its
    /// history is still there to explain what the agent used to believe.
    fn forgetting_a_memory_hides_it_from_the_projection_but_keeps_its_history(store: &EventStore) {
        let sid = store.create_session("m").unwrap();
        let (cited, turn_id) = cited_event(store, &sid);
        let scope = MemoryScope::Project {
            key: "flux".to_string(),
        };

        let doomed = store
            .remember(
                &scope,
                note_citing("auth middleware is in src/auth.rs", &cited, Some(turn_id)),
            )
            .unwrap();
        let keeper = store
            .remember(
                &scope,
                note_citing("tests live under crates/*/tests", &cited, Some(turn_id)),
            )
            .unwrap();

        // An edit APPENDS: the projection shows the new claim, in the entry's original position.
        let edited = store
            .amend_memory(
                &scope,
                &doomed.id,
                note_citing(
                    "auth middleware moved to src/mw/auth.rs",
                    &cited,
                    Some(turn_id),
                ),
            )
            .unwrap()
            .expect("amending a believed entry must succeed");
        assert_eq!(edited.id, doomed.id, "an edit keeps the entry id stable");
        let live = store.memories(&scope).unwrap();
        assert_eq!(
            live.iter().map(|m| m.claim.as_str()).collect::<Vec<_>>(),
            vec![
                "auth middleware moved to src/mw/auth.rs",
                "tests live under crates/*/tests"
            ],
            "the edit must replace the entry in place, not re-order the projection"
        );

        // A forget APPENDS a tombstone; the entry leaves the projection.
        assert!(store.forget_memory(&scope, &doomed.id).unwrap());
        let live = store.memories(&scope).unwrap();
        assert_eq!(
            live.iter().map(|m| m.id.clone()).collect::<Vec<_>>(),
            vec![keeper.id.clone()],
            "a forgotten entry must disappear from the projection"
        );

        // …while its whole believed history remains in the log: the original claim, the edit, and
        // the tombstone are all still readable.
        let history = store.memory_history(&scope).unwrap();
        let names: Vec<&str> = history
            .iter()
            .filter_map(|e| match &e.kind {
                EventKind::Custom { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            names,
            vec![
                memory::MEMORY_NOTED,
                memory::MEMORY_NOTED,
                memory::MEMORY_EDITED,
                memory::MEMORY_FORGOTTEN
            ],
            "nothing may be rewritten or deleted: edit and forget are appends"
        );
        let claims: Vec<String> = history
            .iter()
            .filter_map(|e| match &e.kind {
                EventKind::Custom { name, payload } if name == memory::MEMORY_NOTED => {
                    serde_json::from_value::<MemoryEntry>(payload.clone())
                        .ok()
                        .map(|m| m.claim)
                }
                _ => None,
            })
            .collect();
        assert!(
            claims.contains(&"auth middleware is in src/auth.rs".to_string()),
            "the superseded claim must survive in the log: {claims:?}"
        );

        // Forgetting twice is a no-op, and so is forgetting an unknown id — nothing extra appended.
        assert!(!store.forget_memory(&scope, &doomed.id).unwrap());
        assert!(!store.forget_memory(&scope, "not-an-entry").unwrap());
        assert!(store
            .amend_memory(&scope, &doomed.id, note_citing("back again", &cited, None))
            .unwrap()
            .is_none());
        assert_eq!(store.memory_history(&scope).unwrap().len(), 4);
    }

    /// A-107 item 2: the receipt cites the **stable event id** (a ULID), never `global_seq`, and it
    /// resolves back to exactly the cited event after a store round-trip.
    fn a_receipt_cites_the_stable_event_id_not_the_backend_rowid(store: &EventStore) {
        let sid = store.create_session("m").unwrap();
        let (cited, turn_id) = cited_event(store, &sid);
        let scope = MemoryScope::Global;

        let entry = store
            .remember(
                &scope,
                note_citing(
                    "auth middleware is in src/mw/auth.rs",
                    &cited,
                    Some(turn_id),
                ),
            )
            .unwrap();

        assert_eq!(entry.receipt.event_id, cited.id);
        assert_ne!(
            entry.receipt.event_id,
            cited.global_seq.to_string(),
            "the citation must not be the backend rowid"
        );
        assert!(
            ulid::Ulid::from_string(&entry.receipt.event_id).is_ok(),
            "a store-minted event id is a ULID: {}",
            entry.receipt.event_id
        );
        assert_eq!(entry.receipt.turn_id, Some(turn_id));

        // Round-trip through the log (serialize → store → decode → project) and the citation still
        // resolves to the same event.
        let read_back = store
            .memories(&scope)
            .unwrap()
            .into_iter()
            .find(|m| m.id == entry.id)
            .expect("the entry must project back out of the log");
        assert_eq!(read_back.receipt, entry.receipt);
        let resolved = store
            .resolve_receipt(&read_back.receipt)
            .unwrap()
            .expect("the citation must resolve");
        assert_eq!(resolved.id, cited.id);
        assert_eq!(resolved.global_seq, cited.global_seq);

        // A citation into a stream that isn't here resolves to `None`, never to a wrong event.
        let dangling = Receipt {
            stream: "s_99999".to_string(),
            event_id: cited.id.clone(),
            turn_id: None,
        };
        assert!(store.resolve_receipt(&dangling).unwrap().is_none());
    }

    /// A-107 item 5: a claim carrying a credential shape is scrubbed by the **same `Redactor` the
    /// evidence flush seam uses** (C-22/C-164) before the store ever sees it — both the always-on
    /// credential-shape heuristic and the registered-value path. There is no second scrubber here:
    /// `MemoryNote::new` takes the live redactor's `redact` and is the only way to build a note.
    fn a_credential_shaped_claim_is_scrubbed_before_it_reaches_the_store(store: &EventStore) {
        let sid = store.create_session("m").unwrap();
        let (cited, _turn) = cited_event(store, &sid);
        let scope = MemoryScope::Global;

        // One credential caught by SHAPE alone (never registered anywhere), one caught because it
        // was registered on the live turn's redactor — the two halves of the flush seam.
        let shaped = "sk-ant-api03-AAAABBBBCCCCDDDD";
        let registered = "orchid-galaxy-turnip-9317";
        let redactor = flux_secret::Redactor::new();
        redactor.add_secret(registered.to_string());
        let raw = format!("deploy with {shaped} and the passphrase {registered}");

        let note = MemoryNote::new(
            &raw,
            Receipt {
                stream: cited.stream.clone(),
                event_id: cited.id.clone(),
                turn_id: None,
            },
            None,
            |s| redactor.redact(s),
        );
        let entry = store.remember(&scope, note).unwrap();

        for (label, text) in [
            ("the returned entry", entry.claim.clone()),
            (
                "the projection",
                store.memories(&scope).unwrap()[0].claim.clone(),
            ),
            (
                "the raw stored payload",
                serde_json::to_string(&store.memory_history(&scope).unwrap()[0].kind).unwrap(),
            ),
        ] {
            assert!(
                !text.contains(shaped),
                "{label} must not carry the credential-shaped token: {text}"
            );
            assert!(
                !text.contains(registered),
                "{label} must not carry the registered secret: {text}"
            );
            assert!(
                text.contains("[redacted]"),
                "{label} must show the redaction marker: {text}"
            );
        }
    }

    /// A-107 item 2, the part that actually costs something to get wrong: a citation must still
    /// name **the same event** after a store migration that renumbers every `global_seq`.
    ///
    /// The migration is simulated the way a real one behaves — a fresh store, seeded with unrelated
    /// traffic first so its rowid counter is offset, then both streams re-imported carrying each
    /// event's **stable id** (the one thing a migration preserves) while the destination mints its
    /// own rowids. The last assertion is the point of the whole story item: the source's
    /// `global_seq` no longer names the cited event in the destination, so a receipt pinned to it
    /// would have silently come to cite something else — the worst kind of provenance bug, because
    /// it still looks resolvable.
    #[test]
    fn a_memory_citation_survives_a_migration_that_renumbers_global_seq() {
        let scope = MemoryScope::Global;
        let src = EventStore::in_memory().unwrap();
        let sid = src.create_session("m").unwrap();
        let (cited, _turn) = cited_event(&src, &sid);
        let entry = src
            .remember(
                &scope,
                note_citing("the auth middleware is in src/mw/auth.rs", &cited, None),
            )
            .unwrap();
        let src_rowid = cited.global_seq;

        // The destination: unrelated traffic on an ad-hoc stream first, so every rowid it mints for
        // the imported events lands somewhere the source's numbering never reached.
        let dst = EventStore::in_memory().unwrap();
        for i in 0..7 {
            dst.append(
                "decoy",
                NewEvent::message(Message::user_text(format!("unrelated {i}"))),
            )
            .unwrap();
        }
        let imported_sid = dst.create_session("m").unwrap();
        assert_eq!(
            imported_sid, sid,
            "the fixture assumes the destination re-mints the same session id"
        );
        for stream in [sid.as_str(), scope.stream().as_str()] {
            for ev in src.load_stream(stream, None).unwrap() {
                if matches!(ev.kind, EventKind::SessionStarted { .. }) {
                    continue; // the destination minted its own
                }
                dst.append(stream, NewEvent::new(ev.kind).with_id(ev.id))
                    .unwrap();
            }
        }

        // The entry projects out of the migrated store unchanged, receipt and all…
        let migrated = dst
            .memories(&scope)
            .unwrap()
            .into_iter()
            .find(|m| m.id == entry.id)
            .expect("the migrated entry must project");
        assert_eq!(migrated.receipt, entry.receipt);

        // …and the citation still resolves to the same event, now under a different rowid.
        let resolved = dst
            .resolve_receipt(&migrated.receipt)
            .unwrap()
            .expect("the stable-id citation must still resolve after the migration");
        assert_eq!(resolved.id, cited.id);
        assert_eq!(resolved.kind, cited.kind);
        assert_ne!(
            resolved.global_seq, src_rowid,
            "the fixture must actually renumber the rowid, or it proves nothing"
        );

        // The punchline: the source's `global_seq` does not name the cited event here any more.
        let by_rowid = dst
            .load_stream(&sid, None)
            .unwrap()
            .into_iter()
            .find(|e| e.global_seq == src_rowid);
        assert!(
            by_rowid.is_none_or(|e| e.id != cited.id),
            "a global_seq-pinned citation would now resolve to the wrong event"
        );
    }

    /// The driver-free backend (C-274): every backend-agnostic conformance body again, against a
    /// fresh [`EventStore::ephemeral`]. This is what makes the `--no-default-features` build a
    /// portability win rather than a hollow one — the claim "feature-off still has a usable
    /// `EventBackend`" is only worth as much as the suite behind it, so it is held to exactly the
    /// same bodies as SQLite and Postgres, in the DEFAULT build (the backend is always compiled).
    mod ephemeral_tests {
        use super::*;

        /// Run a conformance body against a fresh driver-free store.
        macro_rules! ephemeral_case {
            ($name:ident) => {
                #[test]
                fn $name() {
                    super::$name(&EventStore::ephemeral());
                }
            };
        }

        ephemeral_case!(create_append_load_roundtrip);
        ephemeral_case!(updated_at_advances_on_append);
        ephemeral_case!(updated_at_advances_on_set_model);
        ephemeral_case!(conversation_delta_folds_incrementally_like_a_full_replay);
        ephemeral_case!(compaction_replaces_the_live_view_but_keeps_history);
        ephemeral_case!(latest_session_tracks_newest);
        ephemeral_case!(unknown_session_has_no_conversation_but_info_errors);
        ephemeral_case!(list_returns_newest_first_with_counts);
        ephemeral_case!(search_selects_matching_sessions_and_excludes_others);
        ephemeral_case!(search_query_cannot_recover_a_redacted_secret);
        ephemeral_case!(set_model_updates_listing);
        ephemeral_case!(find_correlated_returns_newest_matching_tagged_stream);
        ephemeral_case!(find_correlated_in_realm_isolates_realms);
        ephemeral_case!(find_correlated_in_realm_never_matches_legacy_untagged_sessions);
        ephemeral_case!(find_correlated_in_realm_returns_newest_within_the_realm);
        ephemeral_case!(record_call_usage_is_turn_scoped_and_a_noop_on_negative_turn_id);
        ephemeral_case!(cost_summary_wraps_the_projection_over_one_stream);
        ephemeral_case!(cost_summary_all_aggregates_across_sessions);
        ephemeral_case!(cost_summary_for_account_scopes_and_sums_through_the_shared_fold);
        ephemeral_case!(prune_empty_removes_zero_message_sessions);
        ephemeral_case!(prune_empty_excluding_preserves_active_empty_sessions);
        ephemeral_case!(prune_inactive_deletes_only_expired_streams_with_the_tag);
        ephemeral_case!(prune_inactive_excluding_protects_the_keep_list);
        ephemeral_case!(prune_empty_keeps_sessions_with_durable_nonmessage_facts);
        ephemeral_case!(projections_read_only_their_kinds_from_a_mixed_stream);
        ephemeral_case!(roles_round_trip_through_the_conversation);
        ephemeral_case!(append_is_transactional_and_sequences_monotonically);
        ephemeral_case!(run_events_and_turn_telemetry_share_the_log);
        ephemeral_case!(idempotent_append_with_a_stable_id);
        ephemeral_case!(context_round_trips_on_stored_events_and_summaries);
        ephemeral_case!(accounts_are_isolated_in_scoped_reads);
        ephemeral_case!(single_tenant_session_has_empty_context);
        ephemeral_case!(cost_summary_all_does_not_double_count_correlated_children);
        ephemeral_case!(custom_events_append_and_read_back_scoped_by_account);
        ephemeral_case!(prune_older_than_deletes_streams_straddling_the_cutoff);
        ephemeral_case!(prune_adhoc_older_than_reaches_only_aged_unregistered_streams);
        ephemeral_case!(prune_adhoc_older_than_never_deletes_cross_session_memory);
        ephemeral_case!(wakeup_schedule_cancel_and_fire_fold_into_pending_correctly);
        ephemeral_case!(cancel_wakeup_is_a_noop_for_an_unknown_or_already_resolved_id);
        ephemeral_case!(due_wakeups_filters_by_fire_at_and_sorts_soonest_first);
        ephemeral_case!(deleting_a_session_clears_its_pending_wakeups);
        ephemeral_case!(memory_entries_live_on_a_scoped_stream_in_the_same_store);
        ephemeral_case!(forgetting_a_memory_hides_it_from_the_projection_but_keeps_its_history);
        ephemeral_case!(a_receipt_cites_the_stable_event_id_not_the_backend_rowid);
        ephemeral_case!(a_credential_shaped_claim_is_scrubbed_before_it_reaches_the_store);

        // --- driver-free specifics: the SQL guarantees this backend has to reproduce by hand ---

        /// The event-export primitive across two driver-free stores: the conversation, the turn
        /// projection and the remapped turn scoping all survive the copy. `copy_session_to` is
        /// covered against SQLite by the standalone tests above; those construct their stores with
        /// `EventStore::in_memory`, so without this the ephemeral backend's `copy_session_atomic`
        /// would go unexercised in the default build.
        #[test]
        fn copy_session_reproduces_the_conversation_and_remaps_turn_scoping() {
            let src = EventStore::ephemeral();
            let sid = src.create_session("model-a").unwrap();
            let turn = src.begin_turn(&sid, "hi", "model-a").unwrap();
            ask(&src, &sid, "hi");
            src.record_call_usage(&sid, turn, "model-a", Usage::default())
                .unwrap();
            src.end_turn(&sid, turn, "answered", 1, "hi back", None)
                .unwrap();

            let dst = EventStore::ephemeral();
            let copied = src.copy_session_to(&sid, &dst).unwrap();

            assert_eq!(
                dst.conversation(&copied)
                    .unwrap()
                    .iter()
                    .map(|m| m.text())
                    .collect::<Vec<_>>(),
                src.conversation(&sid)
                    .unwrap()
                    .iter()
                    .map(|m| m.text())
                    .collect::<Vec<_>>()
            );
            // The turn projection is what proves the remap: a copied `turn_id` still names a
            // `TurnStarted` in the DESTINATION, so the fold sees one complete turn.
            assert_eq!(
                dst.turns(&copied).unwrap().len(),
                src.turns(&sid).unwrap().len()
            );
            // …and every copied event's scoping resolves inside the destination, not the source.
            let new_turn = dst
                .load_by_kind(&copied, "turn_started")
                .unwrap()
                .first()
                .expect("the copied session has a TurnStarted")
                .global_seq;
            assert!(
                dst.load_turn(&copied, new_turn).unwrap().len() > 1,
                "the copied turn must scope more than its own TurnStarted"
            );
        }

        /// D-185 item 3 on the driver-free backend: where the SQL backends get all-or-nothing from a
        /// transaction, this one has to reserve-then-commit. A copy that fails partway (an
        /// unmappable `turn_id`) must leave NOTHING behind — no listed session, and no orphaned
        /// events under the id it would have minted.
        #[test]
        fn a_failed_copy_commits_nothing() {
            let src = EventStore::ephemeral();
            let sid = src.create_session("model-a").unwrap();
            src.append(
                &sid,
                NewEvent::new(EventKind::CallUsage {
                    model: "model-a".into(),
                    usage: Usage::default(),
                })
                .in_turn(999_999),
            )
            .unwrap();

            let dst = EventStore::ephemeral();
            let err = src.copy_session_to(&sid, &dst).unwrap_err();
            assert!(
                err.to_string().contains("999999"),
                "expected the unmappable turn_id in the diagnostic, got: {err}"
            );
            assert_eq!(
                dst.list(10).unwrap(),
                Vec::new(),
                "a failed copy must leave no listed session"
            );
            // The reservation is released, not consumed: the next real session takes the id the
            // failed copy would have used — and finds nothing of the copy's under it.
            let next = dst.create_session("model-b").unwrap();
            assert_eq!(
                dst.load_stream(&next, None)
                    .unwrap()
                    .iter()
                    .map(|e| e.kind.kind_tag())
                    .collect::<Vec<_>>(),
                vec!["session_started"],
                "a failed copy must leave no events under the session id it reserved"
            );
        }

        /// SQLite's `INTEGER PRIMARY KEY AUTOINCREMENT` never reuses a number, and this backend
        /// mirrors that with monotonic counters rather than `max(id) + 1`: a receipt citing a stable
        /// event id must not be able to resolve to a DIFFERENT event after a prune.
        #[test]
        fn session_ids_are_not_reused_after_a_prune() {
            let store = EventStore::ephemeral();
            let first = store.create_session("m").unwrap();
            assert_eq!(store.prune_empty().unwrap(), 1);
            let second = store.create_session("m").unwrap();
            assert_ne!(
                first, second,
                "a pruned session's id must never be handed out again"
            );
        }
    }

    /// The SQLite backend: every backend-agnostic conformance body against a fresh in-memory store
    /// (today's behavior — must stay green), plus the SQLite-specific tests (file reopen, cross-process
    /// busy-timeout) that have no Postgres analog.
    #[cfg(feature = "sqlite")]
    mod sqlite_tests {
        use super::*;

        /// Run a conformance body against a fresh in-memory SQLite store.
        macro_rules! sqlite_case {
            ($name:ident) => {
                #[test]
                fn $name() {
                    super::$name(&EventStore::in_memory().unwrap());
                }
            };
        }

        sqlite_case!(create_append_load_roundtrip);
        sqlite_case!(updated_at_advances_on_append);
        sqlite_case!(updated_at_advances_on_set_model);
        sqlite_case!(conversation_delta_folds_incrementally_like_a_full_replay);
        sqlite_case!(compaction_replaces_the_live_view_but_keeps_history);
        sqlite_case!(latest_session_tracks_newest);
        sqlite_case!(unknown_session_has_no_conversation_but_info_errors);
        sqlite_case!(list_returns_newest_first_with_counts);
        sqlite_case!(search_selects_matching_sessions_and_excludes_others);
        sqlite_case!(search_query_cannot_recover_a_redacted_secret);
        sqlite_case!(set_model_updates_listing);
        sqlite_case!(find_correlated_returns_newest_matching_tagged_stream);
        sqlite_case!(find_correlated_in_realm_isolates_realms);
        sqlite_case!(find_correlated_in_realm_never_matches_legacy_untagged_sessions);
        sqlite_case!(find_correlated_in_realm_returns_newest_within_the_realm);
        sqlite_case!(record_call_usage_is_turn_scoped_and_a_noop_on_negative_turn_id);
        sqlite_case!(cost_summary_wraps_the_projection_over_one_stream);
        sqlite_case!(cost_summary_all_aggregates_across_sessions);
        sqlite_case!(cost_summary_for_account_scopes_and_sums_through_the_shared_fold);
        sqlite_case!(prune_empty_removes_zero_message_sessions);
        sqlite_case!(prune_empty_excluding_preserves_active_empty_sessions);
        sqlite_case!(prune_inactive_deletes_only_expired_streams_with_the_tag);
        sqlite_case!(prune_inactive_excluding_protects_the_keep_list);
        sqlite_case!(prune_empty_keeps_sessions_with_durable_nonmessage_facts);
        sqlite_case!(projections_read_only_their_kinds_from_a_mixed_stream);
        sqlite_case!(roles_round_trip_through_the_conversation);
        sqlite_case!(append_is_transactional_and_sequences_monotonically);
        sqlite_case!(run_events_and_turn_telemetry_share_the_log);
        sqlite_case!(idempotent_append_with_a_stable_id);
        sqlite_case!(context_round_trips_on_stored_events_and_summaries);
        sqlite_case!(accounts_are_isolated_in_scoped_reads);
        sqlite_case!(single_tenant_session_has_empty_context);
        sqlite_case!(cost_summary_all_does_not_double_count_correlated_children);
        sqlite_case!(custom_events_append_and_read_back_scoped_by_account);
        sqlite_case!(prune_older_than_deletes_streams_straddling_the_cutoff);
        sqlite_case!(prune_adhoc_older_than_reaches_only_aged_unregistered_streams);
        sqlite_case!(prune_adhoc_older_than_never_deletes_cross_session_memory);
        sqlite_case!(wakeup_schedule_cancel_and_fire_fold_into_pending_correctly);
        sqlite_case!(cancel_wakeup_is_a_noop_for_an_unknown_or_already_resolved_id);
        sqlite_case!(due_wakeups_filters_by_fire_at_and_sorts_soonest_first);
        sqlite_case!(deleting_a_session_clears_its_pending_wakeups);
        sqlite_case!(memory_entries_live_on_a_scoped_stream_in_the_same_store);
        sqlite_case!(forgetting_a_memory_hides_it_from_the_projection_but_keeps_its_history);
        sqlite_case!(a_receipt_cites_the_stable_event_id_not_the_backend_rowid);
        sqlite_case!(a_credential_shaped_claim_is_scrubbed_before_it_reaches_the_store);

        // --- SQLite-specific: no Postgres analog (file reopen / cross-process busy-timeout) ---

        #[test]
        fn context_survives_reopen() {
            let path = std::env::temp_dir()
                .join(format!("flux-events-d02-reopen-{}.db", std::process::id()));
            let _ = std::fs::remove_file(&path);
            let ctx = EventContext::for_account("tenant-7");
            let id = {
                let store = EventStore::open(&path).unwrap();
                store.create_session_with_context("m", &ctx).unwrap()
            };
            // Reopen: the additive column migration is idempotent (columns already exist) and the
            // context persists across the process boundary.
            let store = EventStore::open(&path).unwrap();
            assert_eq!(store.info(&id).unwrap().context, ctx);
            assert_eq!(store.list_for_account("tenant-7", 10).unwrap()[0].id, id);
            let _ = std::fs::remove_file(&path);
        }

        /// A-98 (failing-first): a registered wake-up survives a real close/reopen of the sqlite
        /// file — the closest a unit test gets to "survives process exit" — and rehydrates with
        /// its `context` intact, byte-for-byte.
        #[test]
        fn wakeup_registration_survives_reopen_with_context_intact() {
            let path = std::env::temp_dir().join(format!(
                "flux-events-a98-wakeup-reopen-{}.db",
                std::process::id()
            ));
            let _ = std::fs::remove_file(&path);
            let (id, wakeup_id) = {
                let store = EventStore::open(&path).unwrap();
                let id = store.create_session("m").unwrap();
                let wakeup_id = store
                    .schedule_wakeup(
                        &id,
                        1_753_000_000_000,
                        "check the deploy finished",
                        Some("deploy id: abc123, started by turn 4"),
                    )
                    .unwrap();
                (id, wakeup_id)
            };
            // Reopen: a fresh `EventStore` handle on the SAME file, as a new process would get.
            let store = EventStore::open(&path).unwrap();
            let pending = store.pending_wakeups(&id).unwrap();
            assert_eq!(pending.len(), 1, "the registration survived the reopen");
            assert_eq!(pending[0].wakeup_id, wakeup_id);
            assert_eq!(pending[0].fire_at_ms, 1_753_000_000_000);
            assert_eq!(pending[0].prompt, "check the deploy finished");
            assert_eq!(
                pending[0].context.as_deref(),
                Some("deploy id: abc123, started by turn 4"),
                "context rehydrates intact, not just the prompt"
            );
            let _ = std::fs::remove_file(&path);
        }

        /// C-25: two `EventStore` handles on the same file (a `serve` daemon + a CLI turn on the shared
        /// `~/.flux/events.db`) must not lose a write to `SQLITE_BUSY`. WAL permits one writer at a time;
        /// here a raw connection holds the write lock, released after a short delay. With `busy_timeout`
        /// set the contended writer WAITS for the lock and succeeds; without it the write returns
        /// `SQLITE_BUSY` immediately and aborts.
        #[test]
        fn concurrent_writers_wait_on_busy_timeout_instead_of_erroring() {
            use std::time::Duration;
            let path = std::env::temp_dir().join(format!(
                "flux-events-c25-busy-{}-{:?}.db",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_file(&path);

            // Set up a session via one handle.
            let s1 = EventStore::open(&path).unwrap();
            let sid = s1.create_session("m").unwrap();

            // A second handle on the SAME file (a separate connection = a separate process's writer).
            let s2 = EventStore::open(&path).unwrap();

            // Hold the single WAL write lock on a raw connection: BEGIN IMMEDIATE acquires it now.
            let blocker = rusqlite::Connection::open(&path).unwrap();
            blocker.execute_batch("BEGIN IMMEDIATE").unwrap();

            // Release the lock after a short delay, from another thread.
            let releaser = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(300));
                blocker.execute_batch("COMMIT").unwrap();
            });

            // With busy_timeout set this append waits ~300ms for the lock and succeeds; without it the
            // write returns SQLITE_BUSY at once → Err.
            let res = s2.append(
                &sid,
                NewEvent::message(Message::user_text("under contention")),
            );
            releaser.join().unwrap();

            assert!(
                res.is_ok(),
                "a contended writer must wait on busy_timeout, not error: {res:?}"
            );
            assert_eq!(s2.conversation(&sid).unwrap().len(), 1, "the write landed");
            let _ = std::fs::remove_file(&path);
        }

        /// Minimal `tracing::Subscriber` that records event messages — just enough to assert C-124's
        /// contention trace without pulling in `tracing-subscriber` (mirrors
        /// `flux_runtime::metadata`'s test-only `RecordingSubscriber`).
        struct RecordingSubscriber {
            messages: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        }

        struct MessageVisitor<'a>(&'a mut String);

        impl tracing::field::Visit for MessageVisitor<'_> {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    *self.0 = format!("{value:?}");
                }
            }
        }

        impl tracing::Subscriber for RecordingSubscriber {
            fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
                true
            }
            fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
                tracing::span::Id::from_u64(1)
            }
            fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
            fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {
            }
            fn event(&self, event: &tracing::Event<'_>) {
                let mut message = String::new();
                event.record(&mut MessageVisitor(&mut message));
                self.messages.lock().unwrap().push(message);
            }
            fn enter(&self, _span: &tracing::span::Id) {}
            fn exit(&self, _span: &tracing::span::Id) {}
        }

        /// C-124: an append that waits on the SQLite write lock past the warn threshold must leave a
        /// visible trace. Same second-connection-holds-the-lock mechanism as
        /// `concurrent_writers_wait_on_busy_timeout_instead_of_erroring` above, but the contended
        /// handle is opened with a test-only, deliberately low threshold (`SqliteEvents` normally
        /// warns past ~1s) so the test proves the trace deterministically without sleeping a full
        /// second: the lock is held for a fixed, short delay comfortably past the low threshold.
        #[test]
        fn append_that_waits_on_the_busy_handler_leaves_a_warn_trace() {
            use std::time::Duration;
            let path = std::env::temp_dir().join(format!(
                "flux-events-c124-contention-{}-{:?}.db",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_file(&path);

            let s1 = EventStore::open(&path).unwrap();
            let sid = s1.create_session("m").unwrap();

            // The contended handle: same file, but a 20ms warn threshold instead of production's ~1s,
            // so a deterministic ~150ms lock hold below proves the trace fast.
            let s2 = EventStore {
                backend: Backend::Sqlite(
                    SqliteEvents::open_with_contention_threshold(&path, Duration::from_millis(20))
                        .unwrap(),
                ),
            };

            let blocker = rusqlite::Connection::open(&path).unwrap();
            blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
            let releaser = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(150));
                blocker.execute_batch("COMMIT").unwrap();
            });

            let messages = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let subscriber = RecordingSubscriber {
                messages: messages.clone(),
            };
            let res = tracing::subscriber::with_default(subscriber, || {
                s2.append(
                    &sid,
                    NewEvent::message(Message::user_text("under contention")),
                )
            });
            releaser.join().unwrap();

            assert!(
                res.is_ok(),
                "the contended write must still succeed: {res:?}"
            );
            let captured = messages.lock().unwrap();
            assert!(
                captured.iter().any(|m| m.contains("wait")),
                "expected a trace naming the wait on the write lock, got: {captured:?}"
            );
            let _ = std::fs::remove_file(&path);
        }

        /// C-124's flip side: a store with production's default threshold and no contention emits no
        /// new trace at all — the seam is measurement-only noise on uncontended appends.
        #[test]
        fn uncontended_append_emits_no_contention_warning() {
            let store = EventStore::in_memory().unwrap();
            let sid = store.create_session("m").unwrap();

            let messages = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let subscriber = RecordingSubscriber {
                messages: messages.clone(),
            };
            tracing::subscriber::with_default(subscriber, || {
                store
                    .append(
                        &sid,
                        NewEvent::message(Message::user_text("no contention here")),
                    )
                    .unwrap();
            });

            let captured = messages.lock().unwrap();
            assert!(
                captured.is_empty(),
                "an uncontended append must emit no contention trace, got: {captured:?}"
            );
        }

        /// C-126 (failing-first): the design doc §4.3 premise — a long-lived process that always
        /// holds a read snapshot open (a streamed A2A subscription, a held session cursor, …) can
        /// defer WAL checkpointing indefinitely, so `events.db-wal` keeps growing. This proves
        /// both halves of the story in one scenario: while the reader stays pinned, the
        /// `checkpoint` hook must neither block nor error (it has nothing safe to reclaim, so it
        /// silently skips); once the reader releases, the very next call actually shrinks the
        /// sidecar back down.
        #[test]
        fn checkpoint_hook_shrinks_the_wal_once_a_pinned_reader_releases_it() {
            use std::time::Duration;
            let path = std::env::temp_dir().join(format!(
                "flux-events-c126-checkpoint-{}-{:?}.db",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_file(&path);
            let wal_path = format!("{}-wal", path.display());

            let store = EventStore::open(&path).unwrap();
            let sid = store.create_session("m").unwrap();

            // Pin a read snapshot on a SEPARATE connection, exactly like a long-lived reader
            // would: `BEGIN` alone takes no lock, an actual read inside it is what pins the WAL
            // snapshot SQLite's own autocheckpoint (and our hook) must not discard.
            let reader = rusqlite::Connection::open(&path).unwrap();
            reader.execute_batch("BEGIN").unwrap();
            let _pin: i64 = reader
                .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
                .unwrap();

            // Grow the sidecar visibly while the reader stays pinned.
            let big_text = "x".repeat(4096);
            for i in 0..200 {
                store
                    .append(
                        &sid,
                        NewEvent::message(Message::user_text(format!("{i}:{big_text}"))),
                    )
                    .unwrap();
            }
            let grown = std::fs::metadata(&wal_path).unwrap().len();
            assert!(
                grown > 200_000,
                "WAL should have grown past ~200KB with the reader pinned, got {grown} bytes"
            );

            // The hook must not block or error while the reader is STILL pinned.
            let start = std::time::Instant::now();
            store
                .checkpoint()
                .expect("checkpoint must never error under contention");
            assert!(
                start.elapsed() < Duration::from_millis(500),
                "checkpoint must not block on a pinned reader, took {:?}",
                start.elapsed()
            );
            let still_pinned = std::fs::metadata(&wal_path).unwrap().len();
            assert!(
                still_pinned >= grown,
                "a busy checkpoint must not discard anything the pinned reader still needs"
            );

            // Release the pin — now the hook can actually reclaim + truncate the sidecar.
            reader.execute_batch("COMMIT").unwrap();
            drop(reader);
            store.checkpoint().unwrap();
            let shrunk = std::fs::metadata(&wal_path).unwrap().len();
            assert!(
                shrunk < grown / 10,
                "checkpoint should shrink the WAL once nothing pins it: {shrunk} vs {grown}"
            );

            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(&wal_path);
            let _ = std::fs::remove_file(format!("{}-shm", path.display()));
        }

        /// A connection that reports WAL write-lock contention instead of waiting it out — the
        /// probe [`write_lock_is_held`] answers from. `busy_timeout` must be set to zero
        /// explicitly: rusqlite installs a **5s** default on every `Connection::open`, so a probe
        /// left at the default would sit on the lock for five seconds before answering (and would
        /// still answer correctly, which is exactly how it would go unnoticed).
        fn non_waiting_connection(path: &std::path::Path) -> rusqlite::Connection {
            let conn = rusqlite::Connection::open(path).unwrap();
            conn.busy_timeout(std::time::Duration::ZERO).unwrap();
            conn
        }

        /// Is this connection's `BEGIN IMMEDIATE` refused right now because someone else holds the
        /// WAL write lock? The probe does not wait (see [`non_waiting_connection`]), so this
        /// answers from the lock's *current* state. `Ok(())` means the probe took the lock instead
        /// — it is rolled back, so the probe never disturbs what it observes.
        fn write_lock_is_held(probe: &rusqlite::Connection) -> bool {
            match probe.execute_batch("BEGIN IMMEDIATE") {
                Err(rusqlite::Error::SqliteFailure(err, _))
                    if err.code == rusqlite::ErrorCode::DatabaseBusy =>
                {
                    true
                }
                Ok(()) => {
                    probe.execute_batch("ROLLBACK").unwrap();
                    false
                }
                Err(e) => panic!("the write-lock probe failed for an unexpected reason: {e:?}"),
            }
        }

        /// C-126's other contention case: an ACTIVE WRITER (not just a pinned reader) holding the
        /// WAL write lock. `checkpoint` must return without error and **without waiting the writer
        /// out** — never behind (or itself causing) the 5s busy_timeout ordinary appends use.
        ///
        /// C-253: this used to assert `elapsed < 500ms`, which measured the machine rather than the
        /// code — it reddened the gate at 918ms during an integration run with four sibling `cargo`
        /// builds saturating the CPU, on no defect. A duration was only ever a proxy anyway, so the
        /// test now proves the same property causally, with no clock in it at all:
        ///
        /// 1. The checkpoint connection's busy timeout is zero, so it has no busy handler and
        ///    therefore *cannot* wait for a contended lock — SQLite hands it `SQLITE_BUSY` on the
        ///    spot. `busy_timeout` is the only waiting mechanism this crate installs anywhere, so
        ///    this is the whole of "the checkpoint cannot block". Note zero is **not** the default
        ///    state: rusqlite sets 5s on every `Connection::open`, so dropping the explicit
        ///    `busy_timeout(ZERO)` in `SqliteEvents::open_with_threshold` silently restores a 5s
        ///    wait — which is precisely the regression this step catches. Asserted **first**, which
        ///    is also what makes the rest of the test safe to run inline: a reintroduced wait is red
        ///    here, before anything can sit on a lock.
        /// 2. Under that guarantee, the contended `checkpoint()` still returns `Ok`.
        /// 3. At the moment it returned, the writer was **still holding** the lock — so the
        ///    checkpoint demonstrably did not wait for the writer to release. That is the
        ///    happens-before the 500ms bound was standing in for, asserted directly.
        ///
        /// The probe is checked against a released lock too (step 4), so a probe that had somehow
        /// degraded into always answering "held" cannot quietly pass step 3.
        #[test]
        fn checkpoint_hook_never_blocks_or_errors_under_writer_contention() {
            let path = std::env::temp_dir().join(format!(
                "flux-events-c126-writer-contention-{}-{:?}.db",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_file(&path);

            let store = EventStore::open(&path).unwrap();
            store.create_session("m").unwrap();

            // 1. The checkpoint connection cannot wait for a lock, by construction.
            let Backend::Sqlite(sqlite) = &store.backend else {
                panic!("EventStore::open must yield the sqlite backend");
            };
            assert_eq!(
                sqlite.checkpoint_busy_timeout_ms().unwrap(),
                Some(0),
                "the checkpoint connection must keep a zero busy timeout — with a busy handler \
                 installed a contended checkpoint would wait the writer out instead of giving up"
            );

            // Hold the WAL write lock from a separate connection, as a concurrent writer process
            // would (mirrors `concurrent_writers_wait_on_busy_timeout_instead_of_erroring` above).
            let writer = rusqlite::Connection::open(&path).unwrap();
            writer.execute_batch("BEGIN IMMEDIATE").unwrap();

            let probe = non_waiting_connection(&path);
            assert!(
                write_lock_is_held(&probe),
                "the writer's BEGIN IMMEDIATE must actually hold the WAL write lock, or there is \
                 no contention to test"
            );

            // 2. Contended, the checkpoint reports success rather than an error.
            let result = store.checkpoint();
            assert!(
                result.is_ok(),
                "a checkpoint contended by an active writer must not error: {result:?}"
            );

            // 3. …and it got here while the writer still held the lock, so it cannot have waited
            //    for the writer to release. The writer is only committed below, after this.
            assert!(
                write_lock_is_held(&probe),
                "checkpoint returned only after the writer released the WAL write lock — it waited \
                 the writer out instead of giving up immediately"
            );

            // 4. The probe is a real detector, not a constant: once the writer commits it reports
            //    the lock free again.
            writer.execute_batch("COMMIT").unwrap();
            assert!(
                !write_lock_is_held(&probe),
                "the write-lock probe must report the lock free once the writer commits"
            );

            let _ = std::fs::remove_file(&path);
            let _ = std::fs::remove_file(format!("{}-wal", path.display()));
            let _ = std::fs::remove_file(format!("{}-shm", path.display()));
        }

        /// The interactive-CLI-unchanged half of C-126's Acceptance: [`EventStore::in_memory`] has
        /// no WAL sidecar, so `checkpoint` is a plain no-op — never an error, never a panic.
        #[test]
        fn checkpoint_hook_is_a_harmless_noop_for_an_in_memory_store() {
            let store = EventStore::in_memory().unwrap();
            store.create_session("m").unwrap();
            store.checkpoint().unwrap();
        }
    }

    /// The Postgres backend: every backend-agnostic conformance body against a throwaway schema on
    /// the DSN in `TEST_POSTGRES_URL`. Skipped with a notice (never failed) when that env var is
    /// unset, so the default `cargo test` stays DB-free. Each test isolates itself in its own
    /// `?schema=t_<ulid>`, so the whole module is parallel-safe.
    #[cfg(feature = "postgres")]
    mod postgres_tests {
        use super::*;
        use flux_pg::{sqlx, PgHandle};

        /// A fresh `EventStore` in a throwaway schema, or `None` when `TEST_POSTGRES_URL` is unset.
        fn pg_store() -> Option<EventStore> {
            let base = std::env::var("TEST_POSTGRES_URL").ok()?;
            let schema = format!("t_{}", ulid::Ulid::generate().to_string().to_lowercase());
            let sep = if base.contains('?') { '&' } else { '?' };
            let url = format!("{base}{sep}schema={schema}");
            Some(EventStore::open_postgres(PgHandle::connect(&url).unwrap()).unwrap())
        }

        /// Run a conformance body against a throwaway-schema Postgres store, or skip with a notice.
        macro_rules! pg_case {
            ($name:ident) => {
                #[test]
                fn $name() {
                    let Some(store) = pg_store() else {
                        eprintln!(concat!(
                            "skipping ",
                            stringify!($name),
                            ": TEST_POSTGRES_URL unset"
                        ));
                        return;
                    };
                    super::$name(&store);
                }
            };
        }

        pg_case!(create_append_load_roundtrip);
        pg_case!(updated_at_advances_on_append);
        pg_case!(updated_at_advances_on_set_model);
        pg_case!(conversation_delta_folds_incrementally_like_a_full_replay);
        pg_case!(compaction_replaces_the_live_view_but_keeps_history);
        pg_case!(latest_session_tracks_newest);
        pg_case!(unknown_session_has_no_conversation_but_info_errors);
        pg_case!(list_returns_newest_first_with_counts);
        pg_case!(search_selects_matching_sessions_and_excludes_others);
        pg_case!(search_query_cannot_recover_a_redacted_secret);
        pg_case!(set_model_updates_listing);
        pg_case!(find_correlated_returns_newest_matching_tagged_stream);
        pg_case!(find_correlated_in_realm_isolates_realms);
        pg_case!(find_correlated_in_realm_never_matches_legacy_untagged_sessions);
        pg_case!(find_correlated_in_realm_returns_newest_within_the_realm);
        pg_case!(record_call_usage_is_turn_scoped_and_a_noop_on_negative_turn_id);
        pg_case!(cost_summary_wraps_the_projection_over_one_stream);
        pg_case!(cost_summary_all_aggregates_across_sessions);
        pg_case!(cost_summary_for_account_scopes_and_sums_through_the_shared_fold);
        pg_case!(prune_empty_removes_zero_message_sessions);
        pg_case!(prune_empty_excluding_preserves_active_empty_sessions);
        pg_case!(prune_inactive_deletes_only_expired_streams_with_the_tag);
        pg_case!(prune_inactive_excluding_protects_the_keep_list);
        pg_case!(prune_empty_keeps_sessions_with_durable_nonmessage_facts);
        pg_case!(projections_read_only_their_kinds_from_a_mixed_stream);
        pg_case!(roles_round_trip_through_the_conversation);
        pg_case!(append_is_transactional_and_sequences_monotonically);
        pg_case!(run_events_and_turn_telemetry_share_the_log);
        pg_case!(idempotent_append_with_a_stable_id);
        pg_case!(context_round_trips_on_stored_events_and_summaries);
        pg_case!(accounts_are_isolated_in_scoped_reads);
        pg_case!(single_tenant_session_has_empty_context);
        pg_case!(cost_summary_all_does_not_double_count_correlated_children);
        pg_case!(custom_events_append_and_read_back_scoped_by_account);
        pg_case!(prune_older_than_deletes_streams_straddling_the_cutoff);
        pg_case!(prune_adhoc_older_than_reaches_only_aged_unregistered_streams);
        pg_case!(prune_adhoc_older_than_never_deletes_cross_session_memory);
        pg_case!(wakeup_schedule_cancel_and_fire_fold_into_pending_correctly);
        pg_case!(cancel_wakeup_is_a_noop_for_an_unknown_or_already_resolved_id);
        pg_case!(due_wakeups_filters_by_fire_at_and_sorts_soonest_first);
        pg_case!(deleting_a_session_clears_its_pending_wakeups);
        pg_case!(memory_entries_live_on_a_scoped_stream_in_the_same_store);
        pg_case!(forgetting_a_memory_hides_it_from_the_projection_but_keeps_its_history);
        pg_case!(a_receipt_cites_the_stable_event_id_not_the_backend_rowid);
        pg_case!(a_credential_shaped_claim_is_scrubbed_before_it_reaches_the_store);

        /// D-76 (PG-only): eight simultaneous cold-boots against ONE fresh schema must ALL succeed.
        /// Postgres's `IF NOT EXISTS` DDL is not atomic — without the global `flux:ddl` advisory
        /// lock taken first in `PgEvents::connect`'s bootstrap transaction, concurrent first-boots
        /// race the catalog insert and the loser errors (`pg_type_typname_nsp_index` duplicate
        /// key). The race is probabilistic and does not reliably reproduce against a local server,
        /// so this is a regression tripwire rather than a deterministic red/green proof.
        #[test]
        fn concurrent_cold_boots_serialize_bootstrap_ddl() {
            let Ok(base) = std::env::var("TEST_POSTGRES_URL") else {
                eprintln!(
                    "skipping concurrent_cold_boots_serialize_bootstrap_ddl: TEST_POSTGRES_URL unset"
                );
                return;
            };
            let schema = format!("t_{}", ulid::Ulid::generate().to_string().to_lowercase());
            let sep = if base.contains('?') { '&' } else { '?' };
            let url = format!("{base}{sep}schema={schema}");

            const N: usize = 8;
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(N));
            let threads: Vec<_> = (0..N)
                .map(|_| {
                    let url = url.clone();
                    let barrier = barrier.clone();
                    // Each thread is its own "replica": its own PgHandle (own pool + runtime)
                    // against the SAME fresh schema. `PgHandle::connect` is lazy — the schema
                    // create + bootstrap DDL fire inside `open_postgres` — so the barrier
                    // releases all eight bootstraps simultaneously.
                    std::thread::spawn(move || -> std::result::Result<(), String> {
                        let handle = PgHandle::connect(&url).map_err(|e| e.to_string())?;
                        barrier.wait();
                        let store = EventStore::open_postgres(handle).map_err(|e| e.to_string())?;
                        // The store must be usable, not merely constructed.
                        store.create_session("m").map_err(|e| e.to_string())?;
                        Ok(())
                    })
                })
                .collect();
            let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();

            // Drop the throwaway schema BEFORE asserting, so a red run doesn't leak it.
            let admin = PgHandle::connect(&base).unwrap();
            let pool = admin.pool().clone();
            let drop_sql = format!("DROP SCHEMA IF EXISTS \"{schema}\" CASCADE");
            admin
                .block_on(async move {
                    // SqlSafeStr: the schema identifier is minted from a ULID above, not user input.
                    sqlx::raw_sql(sqlx::AssertSqlSafe(drop_sql))
                        .execute(&pool)
                        .await
                        .map(|_| ())
                        .map_err(|e| flux_core::Error::Other(format!("drop schema: {e}")))
                })
                .unwrap();

            for (i, r) in results.iter().enumerate() {
                assert!(r.is_ok(), "cold-boot {i} failed: {r:?}");
            }
        }

        /// PG-only: N concurrent appends to ONE stream from a multi-thread tokio context must yield
        /// contiguous `stream_seq`s — no gaps, no duplicates, no panic — proving the per-stream
        /// `pg_advisory_xact_lock` and the flux-pg sync↔async bridge together.
        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn concurrent_appends_to_one_stream_are_contiguous() {
            let Some(store) = pg_store() else {
                eprintln!(
                    "skipping concurrent_appends_to_one_stream_are_contiguous: TEST_POSTGRES_URL unset"
                );
                return;
            };
            let store = std::sync::Arc::new(store);
            let sid = store.create_session("m").unwrap();

            const N: usize = 16;
            let mut handles = Vec::new();
            for i in 0..N {
                let store = store.clone();
                let sid = sid.clone();
                // spawn_blocking: each append drives the sync bridge (block_on) from a tokio worker —
                // exactly the context that panics the naive sync-over-async bridges.
                handles.push(tokio::task::spawn_blocking(move || {
                    store
                        .append(&sid, NewEvent::message(Message::user_text(format!("m{i}"))))
                        .unwrap()
                        .stream_seq
                }));
            }
            let mut seqs = Vec::new();
            for h in handles {
                seqs.push(h.await.unwrap());
            }
            seqs.sort_unstable();
            // SessionStarted holds stream_seq 0; the N concurrent appends must fill 1..=N with no
            // gaps or duplicates — the advisory lock serialized them under one contiguous sequence.
            assert_eq!(
                seqs,
                (1..=N as i64).collect::<Vec<_>>(),
                "contiguous stream_seqs, no gaps or duplicates: {seqs:?}"
            );

            // The final head-check + store teardown run on a blocking thread on purpose: the store
            // owns the flux-pg `tokio::runtime::Runtime`, and dropping a runtime from within an async
            // context panics ("Cannot drop a runtime …"). Moving the last `Arc` into `spawn_blocking`
            // drops it off the async worker. (`store` is the sole remaining ref — every per-append
            // clone was dropped as its task completed above.)
            tokio::task::spawn_blocking(move || {
                assert_eq!(store.head_seq(&sid).unwrap(), N as i64);
            })
            .await
            .unwrap();
        }
    }
}
