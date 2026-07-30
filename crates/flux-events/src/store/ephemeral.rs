//! The driver-free backend — a process-lifetime event store with no database driver behind it.
//!
//! C-274. The default backend links `rusqlite`, which links a C library, so it cannot build for
//! `wasm32-unknown-unknown`; the `sqlite` feature exists so that dependency can be shed. But a
//! feature-off build that compiles and then cannot construct *any* store is not a portability win —
//! `EventBackend` is crate-private (see the [`super`] module header for why the public API stays a
//! concrete struct), so an embedder cannot supply its own implementation from outside. This is that
//! implementation: pure `std` collections behind one `Mutex`, satisfying the same
//! [`EventBackend`](super::EventBackend) contract the SQLite and Postgres backends do, and held to
//! the same shared conformance suite (`ephemeral_case!` in [`super`]'s tests).
//!
//! What it gives up is **durability**, and that is the whole difference: nothing is written anywhere,
//! so the log lives exactly as long as the process. That is why it backs
//! [`EventStore::in_memory`](super::EventStore::in_memory) in a feature-off build (whose contract is
//! already "ephemeral") but never [`EventStore::open`](super::EventStore::open), which promises a
//! file on disk and is therefore gated on the feature that can deliver one.
//!
//! Two SQLite behaviours are mirrored deliberately rather than incidentally:
//!
//! * **Ids are never reused.** `streams.n` and `events.global_seq` are `INTEGER PRIMARY KEY
//!   AUTOINCREMENT` there, so a pruned session's number never comes back. Here they are monotonic
//!   counters for the same reason — a receipt citing a stable event id must not be able to resolve
//!   to a *different* event after a prune.
//! * **A write is all-or-nothing.** Every method takes the lock once and mutates only after the last
//!   fallible step has succeeded, which is what the SQL backends get from a transaction. The
//!   copy-session primitive is the one that needs care: it reserves its ids, builds every row, and
//!   only then commits them, so an unmappable `turn_id` leaves the store untouched.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Mutex;

use flux_core::{Error, Result};

use super::{
    decode_all, now_ms, parse_id, CopyEvent, EventBackend, RawEvent, SessionInfo, SessionSummary,
};
use crate::context::EventContext;
use crate::kind::{EventKind, NewEvent, StoredEvent};

/// One registry row — the `streams` table's columns.
#[derive(Clone)]
struct StreamRow {
    model: String,
    created_at: i64,
    updated_at: i64,
    /// Advances on EVERY append, so `<= 0` means "nothing but the creation event" — the predicate
    /// `prune_empty` gates on (C-87).
    last_seq: i64,
    /// Kept equal to the live (post-compaction) conversation length, so a listing never disagrees
    /// with a replay.
    msg_count: i64,
    context: EventContext,
}

/// One event row — the `events` table's columns, payload still JSON-encoded (decoding is
/// [`decode_all`]'s job, shared by every backend).
#[derive(Clone)]
struct EventRow {
    global_seq: i64,
    stream: String,
    stream_seq: i64,
    id: String,
    kind_tag: &'static str,
    schema_version: u32,
    ts: i64,
    payload: String,
    turn_id: Option<i64>,
}

impl EventRow {
    /// The backend-neutral row tuple [`decode_all`] consumes.
    fn raw(&self) -> RawEvent {
        (
            self.global_seq,
            self.stream_seq,
            self.id.clone(),
            self.schema_version,
            self.ts,
            self.payload.clone(),
            self.turn_id,
        )
    }
}

/// The whole store, guarded as one unit — the `Mutex` plays the role SQLite's write transaction
/// does: a read-then-write primitive (the conversation-head guard, `next_seq`, the registry
/// maintenance) is atomic against every other caller.
struct Inner {
    streams: BTreeMap<i64, StreamRow>,
    events: Vec<EventRow>,
    /// `AUTOINCREMENT` counterparts: monotonic, never reset by a delete.
    next_n: i64,
    next_global_seq: i64,
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            streams: BTreeMap::new(),
            events: Vec::new(),
            // `INTEGER PRIMARY KEY AUTOINCREMENT` hands out 1 first; ids are opaque to callers, but
            // matching the origin keeps a fixture recorded against one backend from reading oddly
            // under the other.
            next_n: 1,
            next_global_seq: 1,
        }
    }
}

impl Inner {
    /// The stream's run context, or empty for an ad-hoc / unknown stream. All events in a stream
    /// share one context, so reads look it up once and stamp every event.
    fn context_of(&self, stream: &str) -> EventContext {
        parse_id(stream)
            .ok()
            .and_then(|n| self.streams.get(&n))
            .map(|s| s.context.clone())
            .unwrap_or_default()
    }

    /// Every row of `stream`, in `stream_seq` order, that satisfies `keep`.
    fn rows_where(&self, stream: &str, keep: impl Fn(&EventRow) -> bool) -> Vec<RawEvent> {
        let mut rows: Vec<&EventRow> = self
            .events
            .iter()
            .filter(|e| e.stream == stream && keep(e))
            .collect();
        rows.sort_by_key(|e| e.stream_seq);
        rows.iter().map(|e| e.raw()).collect()
    }

    /// The newest `stream_seq` in `stream`, or `-1` when it holds nothing.
    fn head_seq(&self, stream: &str) -> i64 {
        self.events
            .iter()
            .filter(|e| e.stream == stream)
            .map(|e| e.stream_seq)
            .max()
            .unwrap_or(-1)
    }

    /// The newest message-affecting `stream_seq` (`message` / `compacted`), or `-1` — A-100's
    /// compare-and-append guard reads this inside the same critical section as the insert.
    fn conversation_head(&self, stream: &str) -> i64 {
        self.events
            .iter()
            .filter(|e| e.stream == stream && matches!(e.kind_tag, "message" | "compacted"))
            .map(|e| e.stream_seq)
            .max()
            .unwrap_or(-1)
    }

    /// An already-stored event with this stable id, for an idempotent retry. Ids are unique across
    /// the whole log (SQLite backs that with `UNIQUE(id)`), so this is not stream-scoped.
    ///
    /// Decodes directly rather than through [`decode_all`] on purpose: that helper *skips* a payload
    /// it cannot read, which would report the event as absent and let a retry store a second row
    /// under the same id. Mirrors `load_by_id` in the SQLite backend, which propagates instead.
    fn by_id(&self, id: &str) -> Result<Option<StoredEvent>> {
        let Some(row) = self.events.iter().find(|e| e.id == id) else {
            return Ok(None);
        };
        Ok(Some(StoredEvent {
            global_seq: row.global_seq,
            stream: row.stream.clone(),
            stream_seq: row.stream_seq,
            id: row.id.clone(),
            turn_id: row.turn_id,
            schema_version: row.schema_version,
            ts_ms: row.ts,
            kind: serde_json::from_str(&row.payload)?,
            context: self.context_of(&row.stream),
        }))
    }

    /// Encode and push one row, minting a ULID id when the event carries none. Returns the
    /// [`StoredEvent`] view of what was stored (`ctx` is surfaced, not persisted — it lives on the
    /// registry row).
    fn push_event(
        &mut self,
        stream: &str,
        ev: &NewEvent,
        stream_seq: i64,
        ctx: &EventContext,
        ts: i64,
    ) -> Result<StoredEvent> {
        let id = ev
            .id
            .clone()
            .unwrap_or_else(|| ulid::Ulid::generate().to_string());
        let payload = serde_json::to_string(&ev.kind)?;
        let global_seq = self.next_global_seq;
        self.next_global_seq += 1;
        self.events.push(EventRow {
            global_seq,
            stream: stream.to_string(),
            stream_seq,
            id: id.clone(),
            kind_tag: ev.kind.kind_tag(),
            schema_version: ev.schema_version,
            ts,
            payload,
            turn_id: ev.turn_id,
        });
        Ok(StoredEvent {
            global_seq,
            stream: stream.to_string(),
            stream_seq,
            id,
            turn_id: ev.turn_id,
            schema_version: ev.schema_version,
            ts_ms: ts,
            kind: ev.kind.clone(),
            context: ctx.clone(),
        })
    }

    /// Mint a registry row and return its number.
    fn insert_stream(&mut self, row: StreamRow) -> i64 {
        let n = self.next_n;
        self.next_n += 1;
        self.streams.insert(n, row);
        n
    }

    /// The registry numbers a prune should take: those matching `predicate`, minus a keep-list. The
    /// keep-list is small (the A-54 in-process live tasks), so filtering it here mirrors what the SQL
    /// backends do in Rust rather than in the query.
    fn expired(&self, keep: &[String], predicate: impl Fn(&StreamRow) -> bool) -> Vec<i64> {
        let keep: HashSet<i64> = keep.iter().filter_map(|s| parse_id(s).ok()).collect();
        self.streams
            .iter()
            .filter(|(n, row)| !keep.contains(n) && predicate(row))
            .map(|(n, _)| *n)
            .collect()
    }

    /// Drop these sessions: their registry rows and every event they hold.
    fn delete_streams(&mut self, ns: &[i64]) {
        let names: HashSet<String> = ns.iter().map(|n| format!("s_{n}")).collect();
        self.events.retain(|e| !names.contains(&e.stream));
        for n in ns {
            self.streams.remove(n);
        }
    }

    /// Registry rows ordered as the listings want them: newest activity first, newest id breaking
    /// a tie (`ORDER BY updated_at DESC, n DESC`).
    fn listing(&self) -> Vec<(i64, &StreamRow)> {
        let mut rows: Vec<(i64, &StreamRow)> = self.streams.iter().map(|(n, s)| (*n, s)).collect();
        rows.sort_by(|a, b| b.1.updated_at.cmp(&a.1.updated_at).then(b.0.cmp(&a.0)));
        rows
    }
}

fn summary(n: i64, row: &StreamRow) -> SessionSummary {
    SessionSummary {
        id: format!("s_{n}"),
        model: row.model.clone(),
        created_at_ms: row.created_at,
        updated_at_ms: row.updated_at,
        messages: row.msg_count as usize,
        context: row.context.clone(),
    }
}

/// The append-only event store with no driver behind it — see the module header.
pub(crate) struct EphemeralEvents {
    inner: Mutex<Inner>,
}

impl EphemeralEvents {
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
        }
    }

    /// The shared write body of [`EventBackend::append`] and A-100's guarded append: when
    /// `expected_head` is `Some`, the conversation head is compared **inside** the same critical
    /// section as the insert, and a miss appends nothing (`Ok(None)`).
    fn append_guarded(
        &self,
        stream: &str,
        ev: NewEvent,
        ts: i64,
        expected_head: Option<i64>,
    ) -> Result<Option<StoredEvent>> {
        let mut inner = self.inner.lock().unwrap();
        if let Some(id) = &ev.id {
            if let Some(existing) = inner.by_id(id)? {
                return Ok(Some(existing)); // idempotent retry: the event is already stored
            }
        }
        if let Some(expected) = expected_head {
            if inner.conversation_head(stream) != expected {
                return Ok(None);
            }
        }
        let ctx = inner.context_of(stream);
        let next_seq = inner.head_seq(stream) + 1;
        let stored = inner.push_event(stream, &ev, next_seq, &ctx, ts)?;

        // Maintain the session registry — but only for real `s_<n>` sessions. The log accepts any
        // stream string (the interpreter writes run events under ad-hoc ids like `"sess"`), so a
        // non-session stream simply has no registry row to update.
        if let Some(row) = parse_id(stream)
            .ok()
            .and_then(|n| inner.streams.get_mut(&n))
        {
            row.updated_at = stored.ts_ms;
            row.last_seq = next_seq;
            match &ev.kind {
                EventKind::SessionStarted { model } | EventKind::ModelChanged { model } => {
                    row.model = model.clone();
                }
                _ => {}
            }
            match &ev.kind {
                EventKind::Message(_) => row.msg_count += 1,
                EventKind::Compacted { messages } => row.msg_count = messages.len() as i64,
                _ => {}
            }
        }
        Ok(Some(stored))
    }
}

impl EventBackend for EphemeralEvents {
    fn create_session_with_context(&self, model: &str, ctx: &EventContext) -> Result<String> {
        let ts = now_ms();
        let mut inner = self.inner.lock().unwrap();
        let n = inner.insert_stream(StreamRow {
            model: model.to_string(),
            created_at: ts,
            updated_at: ts,
            last_seq: 0,
            msg_count: 0,
            context: ctx.clone(),
        });
        let stream = format!("s_{n}");
        let ev = NewEvent::new(EventKind::SessionStarted {
            model: model.to_string(),
        });
        inner.push_event(&stream, &ev, 0, ctx, now_ms())?;
        Ok(stream)
    }

    fn latest_session(&self) -> Result<Option<String>> {
        let inner = self.inner.lock().unwrap();
        Ok(inner.streams.keys().next_back().map(|n| format!("s_{n}")))
    }

    fn info(&self, stream: &str) -> Result<SessionInfo> {
        let n = parse_id(stream)?;
        let inner = self.inner.lock().unwrap();
        let row = inner
            .streams
            .get(&n)
            .ok_or_else(|| Error::Other(format!("session {stream} not found")))?;
        Ok(SessionInfo {
            id: stream.to_string(),
            model: row.model.clone(),
            created_at_ms: row.created_at,
            updated_at_ms: row.updated_at,
            context: row.context.clone(),
        })
    }

    fn list(&self, limit: usize) -> Result<Vec<SessionSummary>> {
        let inner = self.inner.lock().unwrap();
        Ok(inner
            .listing()
            .into_iter()
            .take(limit)
            .map(|(n, row)| summary(n, row))
            .collect())
    }

    fn list_for_account(&self, account: &str, limit: usize) -> Result<Vec<SessionSummary>> {
        let inner = self.inner.lock().unwrap();
        Ok(inner
            .listing()
            .into_iter()
            .filter(|(_, row)| row.context.account.as_deref() == Some(account))
            .take(limit)
            .map(|(n, row)| summary(n, row))
            .collect())
    }

    fn account_streams(&self, account: &str) -> Result<Vec<String>> {
        let inner = self.inner.lock().unwrap();
        Ok(inner
            .listing()
            .into_iter()
            .filter(|(_, row)| row.context.account.as_deref() == Some(account))
            .map(|(n, _)| format!("s_{n}"))
            .collect())
    }

    fn find_correlated(&self, correlation_id: &str, agent_id: &str) -> Result<Option<String>> {
        let inner = self.inner.lock().unwrap();
        Ok(inner
            .streams
            .iter()
            .rev()
            .find(|(_, row)| {
                row.context.correlation_id.as_deref() == Some(correlation_id)
                    && row.context.agent_id.as_deref() == Some(agent_id)
            })
            .map(|(n, _)| format!("s_{n}")))
    }

    fn find_correlated_in_realm(
        &self,
        correlation_id: &str,
        agent_id: &str,
        realm: &str,
    ) -> Result<Option<String>> {
        let inner = self.inner.lock().unwrap();
        Ok(inner
            .streams
            .iter()
            .rev()
            .find(|(_, row)| {
                row.context.correlation_id.as_deref() == Some(correlation_id)
                    && row.context.agent_id.as_deref() == Some(agent_id)
                    && row.context.account.as_deref() == Some(realm)
            })
            .map(|(n, _)| format!("s_{n}")))
    }

    fn children_of(&self, stream: &str) -> Result<Vec<String>> {
        let inner = self.inner.lock().unwrap();
        Ok(inner
            .streams
            .iter()
            .filter(|(_, row)| row.context.correlation_id.as_deref() == Some(stream))
            .map(|(n, _)| format!("s_{n}"))
            .collect())
    }

    fn prune_empty_excluding(&self, keep: &[String]) -> Result<usize> {
        let mut inner = self.inner.lock().unwrap();
        // C-87: `last_seq <= 0` (only the seq-0 `SessionStarted` was ever appended), NOT
        // `msg_count = 0` — a session can carry durable non-message facts.
        let empty = inner.expired(keep, |row| row.last_seq <= 0);
        inner.delete_streams(&empty);
        Ok(empty.len())
    }

    fn prune_inactive_excluding(
        &self,
        agent_id: &str,
        cutoff_ms: i64,
        keep: &[String],
    ) -> Result<usize> {
        let mut inner = self.inner.lock().unwrap();
        let expired = inner.expired(keep, |row| {
            row.context.agent_id.as_deref() == Some(agent_id) && row.updated_at < cutoff_ms
        });
        inner.delete_streams(&expired);
        Ok(expired.len())
    }

    fn prune_older_than(&self, cutoff_ms: i64) -> Result<usize> {
        let mut inner = self.inner.lock().unwrap();
        let expired = inner.expired(&[], |row| row.updated_at < cutoff_ms);
        inner.delete_streams(&expired);
        Ok(expired.len())
    }

    fn prune_adhoc_older_than(&self, cutoff_ms: i64) -> Result<usize> {
        let mut inner = self.inner.lock().unwrap();
        // D-77: ad-hoc streams only — the ones with no registry row, which the registry-enumerating
        // prunes structurally cannot reach. The horizon is per-stream on the NEWEST event, so a
        // still-active ad-hoc stream keeps its FULL history.
        let registered: HashSet<String> = inner.streams.keys().map(|n| format!("s_{n}")).collect();
        let mut newest: HashMap<&str, i64> = HashMap::new();
        for ev in &inner.events {
            if registered.contains(&ev.stream) {
                continue;
            }
            let slot = newest.entry(ev.stream.as_str()).or_insert(i64::MIN);
            *slot = (*slot).max(ev.ts);
        }
        let expired: HashSet<String> = newest
            .into_iter()
            .filter(|(_, newest)| *newest < cutoff_ms)
            .map(|(stream, _)| stream.to_string())
            .collect();
        inner.events.retain(|e| !expired.contains(&e.stream));
        Ok(expired.len())
    }

    fn append(&self, stream: &str, ev: NewEvent) -> Result<StoredEvent> {
        self.append_guarded(stream, ev, now_ms(), None)?
            .ok_or_else(|| {
                Error::Other("event store: an unguarded append reported a guard miss".into())
            })
    }

    fn append_if_conversation_head(
        &self,
        stream: &str,
        ev: NewEvent,
        expected_seq: i64,
    ) -> Result<Option<StoredEvent>> {
        self.append_guarded(stream, ev, now_ms(), Some(expected_seq))
    }

    fn load_stream(&self, stream: &str, after_seq: Option<i64>) -> Result<Vec<StoredEvent>> {
        let inner = self.inner.lock().unwrap();
        let after = after_seq.unwrap_or(-1);
        let ctx = inner.context_of(stream);
        let raw = inner.rows_where(stream, |e| e.stream_seq > after);
        decode_all(stream, &ctx, raw)
    }

    fn load_by_kind(&self, stream: &str, kind: &str) -> Result<Vec<StoredEvent>> {
        let inner = self.inner.lock().unwrap();
        let ctx = inner.context_of(stream);
        let raw = inner.rows_where(stream, |e| e.kind_tag == kind);
        decode_all(stream, &ctx, raw)
    }

    fn conversation_delta(&self, stream: &str, after_seq: i64) -> Result<Vec<StoredEvent>> {
        let inner = self.inner.lock().unwrap();
        let ctx = inner.context_of(stream);
        let raw = inner.rows_where(stream, |e| {
            e.stream_seq > after_seq && matches!(e.kind_tag, "message" | "compacted")
        });
        decode_all(stream, &ctx, raw)
    }

    fn load_turn(&self, stream: &str, turn_id: i64) -> Result<Vec<StoredEvent>> {
        let inner = self.inner.lock().unwrap();
        let ctx = inner.context_of(stream);
        // The turn's own `TurnStarted` (identified BY its global_seq) plus everything scoped to it.
        let raw = inner.rows_where(stream, |e| {
            e.global_seq == turn_id || e.turn_id == Some(turn_id)
        });
        decode_all(stream, &ctx, raw)
    }

    fn head_seq(&self, stream: &str) -> Result<i64> {
        let inner = self.inner.lock().unwrap();
        Ok(inner.head_seq(stream))
    }

    fn all_streams(&self) -> Result<Vec<String>> {
        let inner = self.inner.lock().unwrap();
        Ok(inner.streams.keys().map(|n| format!("s_{n}")).collect())
    }

    fn streams_with_correlation(&self) -> Result<Vec<(i64, Option<String>)>> {
        let inner = self.inner.lock().unwrap();
        Ok(inner
            .streams
            .iter()
            .map(|(n, row)| (*n, row.context.correlation_id.clone()))
            .collect())
    }

    /// D-185: mint the destination session and append every copied event as ONE unit — an
    /// unmappable `turn_id` must leave NOTHING behind for a retry to trip over. Where the SQL
    /// backends roll a transaction back, this reserves its ids, builds every row, and commits only
    /// after the last fallible step.
    fn copy_session_atomic(&self, info: &SessionInfo, events: Vec<CopyEvent>) -> Result<String> {
        let mut inner = self.inner.lock().unwrap();
        let created_at = info.created_at_ms;
        let updated_at = events.last().map(|e| e.ts_ms).unwrap_or(info.updated_at_ms);

        // Reserved, not yet committed: the ids this copy will occupy if it completes.
        let n = inner.next_n;
        let stream = format!("s_{n}");
        let mut next_global_seq = inner.next_global_seq;
        let mut rows: Vec<EventRow> = Vec::with_capacity(events.len() + 1);
        let mut mint = |ev: &NewEvent, stream_seq: i64, ts: i64| -> Result<i64> {
            let global_seq = next_global_seq;
            next_global_seq += 1;
            rows.push(EventRow {
                global_seq,
                stream: stream.clone(),
                stream_seq,
                id: ev
                    .id
                    .clone()
                    .unwrap_or_else(|| ulid::Ulid::generate().to_string()),
                kind_tag: ev.kind.kind_tag(),
                schema_version: ev.schema_version,
                ts,
                payload: serde_json::to_string(&ev.kind)?,
                turn_id: ev.turn_id,
            });
            Ok(global_seq)
        };

        let started = NewEvent::new(EventKind::SessionStarted {
            model: info.model.clone(),
        });
        mint(&started, 0, created_at)?;

        let mut turn_map: HashMap<i64, i64> = HashMap::new();
        let mut next_seq: i64 = 1;
        let mut msg_count: i64 = 0;
        for ev in events {
            let turn_id = match ev.turn_id {
                Some(old) => Some(turn_map.get(&old).copied().ok_or_else(|| {
                    Error::Other(format!(
                        "copy_session_atomic: event {} references turn {old}, which was never \
                         copied (its TurnStarted must precede every event it scopes)",
                        ev.id
                    ))
                })?),
                None => None,
            };
            let mut new_ev = NewEvent::new(ev.kind.clone());
            new_ev.schema_version = ev.schema_version;
            if let Some(turn_id) = turn_id {
                new_ev = new_ev.in_turn(turn_id);
            }
            let global_seq = mint(&new_ev, next_seq, ev.ts_ms)?;
            if matches!(ev.kind, EventKind::TurnStarted { .. }) {
                turn_map.insert(ev.old_global_seq, global_seq);
            }
            match &ev.kind {
                EventKind::Message(_) => msg_count += 1,
                EventKind::Compacted { messages } => msg_count = messages.len() as i64,
                _ => {}
            }
            next_seq += 1;
        }

        // Commit: every fallible step is behind us.
        inner.insert_stream(StreamRow {
            model: info.model.clone(),
            created_at,
            updated_at,
            last_seq: next_seq - 1,
            msg_count,
            context: info.context.clone(),
        });
        inner.next_global_seq = next_global_seq;
        inner.events.extend(rows);
        Ok(stream)
    }
}
