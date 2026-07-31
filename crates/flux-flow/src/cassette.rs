//! C-43: the hermetic-replay **cassette** — record every leaf-op dispatch's (redacted) output
//! durably, and serve it back on replay so a recorded run re-executes with **no model call, no
//! live IO, and no re-fired side effects**.
//!
//! The capture/serve point is the one dispatch chokepoint ([`ExecutorHost::dispatch`]
//! (crate::runtime)); the active scope rides on the [`FlowStore`](crate::state::FlowStore) so
//! every host construction self-wires without signature churn (the A-20 `reads` precedent). Cells
//! land as [`RunEvent::OpRecorded`] on the session's unified event stream — no new table, no new
//! `EventKind` arm, the same `run_trace()` projection replay/fork/diff read.
//!
//! Matching is deliberately NOT a strict next-cell cursor (design review 2026-07-07):
//! - `parallel` branches dispatch concurrently, so record-time interleaving is nondeterministic —
//!   the matcher scans forward for the first **unconsumed** cell matching `(op, hash)`; strictly
//!   sequential plans degenerate to the strict cursor.
//! - the live run hashes the UNredacted input while the cassette serves redacted content, so a
//!   replayed input downstream of a redacted output re-hashes differently — cells carry
//!   `input_hash_redacted` and the matcher accepts either hash (sound because
//!   [`Redactor::redact`] is deterministic longest-first containment replacement, so redaction
//!   commutes with `{{symbol}}` interpolation).
//!
//! D-175 generalizes the two-mode scope into a small family sharing the same dual-hash matcher:
//! [`FrozenTape`] serves a byte-frozen recorded world (with optional substitutions), off a miss
//! either halting or bridging to a live [`RecordScope`]; [`ResumeTape`] serves *completed* ops
//! exactly-once and records a live tail. Both power Tune (D-176) and Resurrect (D-178) without a
//! second dispatch chokepoint — see `runtime.rs`'s `ExecutorHost::dispatch`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use flux_events::{EventStore, NewEvent};
use flux_lang::ast::{RunEvent, StepId};
use flux_lang::host::OpOutcome;
use flux_lang::runtime::sha256_hex;
use flux_secret::Redactor;

/// Capture kill-switch: on by default (without capture there is nothing to replay; every cell is
/// scrubbed through the same redactor as `plan_source`, C-22), disabled with `FLUX_CASSETTE=0`.
pub fn enabled() -> bool {
    std::env::var("FLUX_CASSETTE").map_or(true, |v| v != "0")
}

/// Per-cell content cap (`FLUX_CASSETTE_MAX_BYTES`, default 1 MiB). Unlike `plan_source`'s
/// all-or-nothing drop, an over-cap cell keeps its head with `truncated=true` so divergence
/// detection and rendering still work — but a truncated cell is NOT hermetically replayable and
/// replay refuses it loudly instead of feeding partial bytes.
pub fn max_cell_bytes() -> usize {
    std::env::var("FLUX_CASSETTE_MAX_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1024 * 1024)
}

/// Truncate on a char boundary (never a byte offset — the repo-wide untrusted-bytes invariant).
fn truncate_chars(s: &str, cap: usize) -> (String, bool) {
    if s.len() <= cap {
        return (s.to_string(), false);
    }
    let mut end = cap;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    (s[..end].to_string(), true)
}

/// Redact the dispatch input **after parsing it**, then re-encode. Redacting the serialized JSON
/// directly misses registered values whose spelling changes under JSON escaping (quotes,
/// backslashes, and newlines), which would make `input_view` a recoverable durable secret leak.
///
/// The scrub itself is [`crate::engine::redact_json_in_place`] — the same total walker the evidence
/// flush uses, so no node kind is exempt here either (C-323): a registered all-digit credential
/// arriving as a JSON *number*, or a credential used as an object *key*, is caught.
///
/// **How the rewrite is applied, and why there are two paths.** Patching the original text — which
/// is what this did before C-323 — preserves the caller's field order and whitespace, and that
/// matters because the view is capped and a truncated head is what a person actually reads. But
/// textual substitution is only *safe* for a string leaf: `"…"` is a self-delimiting token, whereas
/// a bare number literal is not, so replacing `216216` inside `1216216789` would splice a quoted
/// string into the middle of a number and leave `input_view` unparseable (the TUI re-parses it).
///
/// So the order-preserving path is kept for the case it was written for — every redaction landing
/// on a string leaf — and a tree that needed a *key* or a *non-string scalar* redacted is re-encoded
/// from the scrubbed value instead. Re-encoding is structurally safe by construction; it costs the
/// caller's key order, and only for a payload that actually carried a credential in one of those
/// positions. An input that needed no redaction at all is returned verbatim.
fn redacted_input_view(redactor: &Redactor, input_json: &str) -> (String, bool) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(input_json) else {
        let redacted = redactor.redact(input_json);
        let changed = redacted != input_json;
        return (redacted, changed);
    };
    let mut scrubbed = value.clone();
    crate::engine::redact_json_in_place(redactor, &mut scrubbed);
    if scrubbed == value {
        return (input_json.to_string(), false);
    }

    // Replace complete encoded string tokens in the original serialization, preserving field order
    // and whitespace — but only when `string_leaf_replacements` confirms every redaction this tree
    // needs is a string leaf.
    let mut replacements = Vec::new();
    if string_leaf_replacements(redactor, &value, &mut replacements) {
        replacements.sort_by_key(|(raw, _)| std::cmp::Reverse(raw.len()));
        replacements.dedup();
        let mut rendered = input_json.to_string();
        for (raw, redacted) in replacements {
            rendered = rendered.replace(&raw, &redacted);
        }
        return (rendered, true);
    }
    match serde_json::to_string(&scrubbed) {
        Ok(rendered) => (rendered, true),
        // Unreachable for a tree that just came out of `from_str`, but a view is never worth a
        // panic — fall back to the whole-text scrub, which is strictly safer than the input.
        Err(_) => (redactor.redact(input_json), true),
    }
}

/// Collect the encoded string tokens to rewrite, and report whether textual substitution is
/// **sufficient** for this tree.
///
/// Returns `false` — and leaves `replacements` unusable — as soon as a redaction is needed that
/// textual substitution cannot express safely: an object *key* (rewriting it in the text would have
/// to move the value with it) or a non-string scalar (no self-delimiting token to replace). The
/// caller re-encodes the scrubbed tree in that case.
fn string_leaf_replacements(
    redactor: &Redactor,
    value: &serde_json::Value,
    replacements: &mut Vec<(String, String)>,
) -> bool {
    match value {
        serde_json::Value::String(text) => {
            let redacted = redactor.redact(text);
            if redacted == *text {
                return true;
            }
            match (
                serde_json::to_string(text),
                serde_json::to_string(&redacted),
            ) {
                (Ok(raw), Ok(redacted)) => {
                    replacements.push((raw, redacted));
                    true
                }
                _ => false,
            }
        }
        serde_json::Value::Array(items) => items
            .iter()
            .all(|item| string_leaf_replacements(redactor, item, replacements)),
        serde_json::Value::Object(fields) => fields.iter().all(|(key, value)| {
            redactor.redact(key) == *key && string_leaf_replacements(redactor, value, replacements)
        }),
        scalar => {
            let literal = scalar.to_string();
            redactor.redact(&literal) == literal
        }
    }
}

/// The active cassette mode, installed on the `FlowStore` by the engine (record, every turn), the
/// replay driver (replay), the fork engine (replay for the prefix, then record for the tail), or a
/// `run_turn_pinned` caller (`Frozen`/`Resume`, D-175). `#[non_exhaustive]`: this enum is public
/// (SemVer-breaking to grow), and every future scope is meant to be additive — the ONE dispatch
/// chokepoint (`runtime.rs`'s `ExecutorHost::dispatch`) matches it exhaustively, so a new arm forces
/// a decision there rather than silently falling through to the live path.
#[non_exhaustive]
pub enum CassetteScope {
    Record(RecordScope),
    Replay(ReplayTape),
    /// Serve from a byte-frozen recorded world, with optional substitutions; a miss either halts or
    /// bridges to live IO, per [`OffTape`]. Tune's world-pinned re-plan, the Test Kit's `check()`.
    Frozen(FrozenTape),
    /// Serve *completed* ops exactly-once from a crash-tail slice, falling through to a live tail
    /// dispatch (recorded for the ledger). Resurrect's crash-resume.
    Resume(ResumeTape),
}

/// How a [`FrozenTape`] behaves when a dispatch has no matching recorded cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OffTape {
    /// Latch a divergence and refuse — the frozen world IS the whole world (Tune's default, the
    /// Test Kit's `check()`). Hermetic by construction: no live IO can ever happen.
    Halt,
    /// Fall through to a live [`RecordScope`] bridge and record the tail — the frozen world is a
    /// PREFIX a re-plan may legitimately read past (world-pinned re-plan under a changed
    /// model/prompt/policy).
    Live,
}

/// Record mode: after every real dispatch, append one redacted [`RunEvent::OpRecorded`] cell to
/// the session's event stream. The UNredacted outcome still flows back to the live interpreter —
/// redaction is for the durable tape only.
pub struct RecordScope {
    events: Arc<EventStore>,
    session: String,
    seq: AtomicU32,
    cap: usize,
}

impl RecordScope {
    pub fn new(events: Arc<EventStore>, session: impl Into<String>) -> Self {
        Self {
            events,
            session: session.into(),
            seq: AtomicU32::new(0),
            cap: max_cell_bytes(),
        }
    }

    /// Append the cell for one completed dispatch. Failures to append are swallowed (recording is
    /// telemetry-grade: it must never fail a live turn), matching `record_plan_attempt`'s posture.
    pub fn record(&self, redactor: &Redactor, op: &str, input_json: &str, outcome: &OpOutcome) {
        let input_hash = sha256_hex(input_json);
        let red_input = redactor.redact(input_json);
        let input_hash_redacted = (red_input != input_json).then(|| sha256_hex(&red_input));
        let (safe_input_view, input_view_redacted) = redacted_input_view(redactor, input_json);
        let (input_view, input_view_truncated) = truncate_chars(&safe_input_view, self.cap);

        let red_content = redactor.redact(&outcome.content);
        let content_redacted = red_content != outcome.content;
        let (content, c_trunc) = truncate_chars(&red_content, self.cap);

        let (view, v_redacted, v_trunc) = match &outcome.view {
            Some(v) => {
                let red = redactor.redact(v);
                let changed = red != *v;
                let (t, tr) = truncate_chars(&red, self.cap);
                (Some(t), changed, tr)
            }
            None => (None, false, false),
        };

        // `redacted` = the scrub changed ANYTHING about this cell — output content/view (rare:
        // the envelope already scrubs tool results before they reach the interpreter, C-13) or
        // the input (common: a plan literal carrying a secret — the case the dual-hash matcher
        // exists for, since `plan_source` persists that literal redacted).
        let cell = RunEvent::OpRecorded {
            seq: self.seq.fetch_add(1, Ordering::SeqCst),
            step: StepId(format!("step_{op}_{}", &input_hash[..16])),
            op: op.to_string(),
            input_hash,
            redacted: content_redacted
                || v_redacted
                || input_hash_redacted.is_some()
                || input_view_redacted,
            input_hash_redacted,
            input_view: Some(input_view),
            input_view_truncated,
            content,
            view,
            is_error: outcome.is_error,
            denied: outcome.denied,
            truncated: c_trunc || v_trunc,
        };
        let _ = self.events.append(&self.session, NewEvent::run(cell));
    }

    /// D-182: every cell already recorded onto this scope's own session, in trace order —
    /// [`crate::whatif::RerunRecordingSink::finish`]'s read-back for reconciling a deferred
    /// self-recording pass against whatever a live bridge already wrote to the SAME target while the
    /// turn/plan ran.
    pub(crate) fn recorded_cells(&self) -> Vec<Cell> {
        self.events
            .run_trace(&self.session)
            .map(|trace| Cell::collect(&trace))
            .unwrap_or_default()
    }
}

/// One recorded cell, hydrated from a session's run trace.
#[derive(Debug, Clone)]
pub struct Cell {
    pub op: String,
    pub input_hash: String,
    pub input_hash_redacted: Option<String>,
    pub content: String,
    pub view: Option<String>,
    pub is_error: bool,
    pub denied: bool,
    pub truncated: bool,
}

impl Cell {
    /// Collect every [`RunEvent::OpRecorded`] cell in a trace slice, in stream order — the shared
    /// extraction [`ReplayTape::from_trace`] and the Resurrect driver's crash-tail slice
    /// (`resurrect.rs`, D-178) both build on, so this shape is derived exactly once.
    pub(crate) fn collect(trace: &[RunEvent]) -> Vec<Cell> {
        trace
            .iter()
            .filter_map(|ev| match ev {
                RunEvent::OpRecorded {
                    op,
                    input_hash,
                    input_hash_redacted,
                    content,
                    view,
                    is_error,
                    denied,
                    truncated,
                    ..
                } => Some(Cell {
                    op: op.clone(),
                    input_hash: input_hash.clone(),
                    input_hash_redacted: input_hash_redacted.clone(),
                    content: content.clone(),
                    view: view.clone(),
                    is_error: *is_error,
                    denied: *denied,
                    truncated: *truncated,
                }),
                _ => None,
            })
            .collect()
    }
}

/// Replay mode: serve cells by `(op, hash)` lookup — the inner executor is never touched for a
/// served op, so no side effect can re-fire. A dispatch with no matching unconsumed cell latches
/// [`ReplayTape::diverged`] and surfaces as a hard in-band error, never silent continuation.
pub struct ReplayTape {
    cells: Vec<Cell>,
    consumed: Mutex<Vec<bool>>,
    diverged: Mutex<Option<String>>,
}

impl ReplayTape {
    /// Collect a recorded trace's cassette cells, in stream order.
    pub fn from_trace(trace: &[RunEvent]) -> Self {
        let cells = Cell::collect(trace);
        let n = cells.len();
        Self {
            cells,
            consumed: Mutex::new(vec![false; n]),
            diverged: Mutex::new(None),
        }
    }

    /// Build a tape from pre-hydrated cells (tests / tooling).
    pub fn from_cells(cells: Vec<Cell>) -> Self {
        let n = cells.len();
        Self {
            cells,
            consumed: Mutex::new(vec![false; n]),
            diverged: Mutex::new(None),
        }
    }

    pub fn len(&self) -> usize {
        self.cells.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// Unconsumed cells remaining (reporting: a clean full replay ends at 0).
    pub fn remaining(&self) -> usize {
        self.consumed
            .lock()
            .unwrap()
            .iter()
            .filter(|c| !**c)
            .count()
    }

    /// The first divergence, if any dispatch failed to match.
    pub fn diverged(&self) -> Option<String> {
        self.diverged.lock().unwrap().clone()
    }

    /// The dual-hash matcher core, shared by [`serve`](Self::serve),
    /// [`serve_nonlatching`](Self::serve_nonlatching), [`FrozenTape`], and [`ResumeTape`] — scans
    /// forward for the first **unconsumed** cell matching `(op, hash)` and, for a live match,
    /// atomically marks it consumed in the same lock acquisition (so two concurrent `parallel`
    /// dispatches can never both claim the same cell). A truncated match is NOT marked consumed —
    /// callers latch and refuse instead of serving partial bytes.
    fn serve_result(&self, op: &str, input_json: &str) -> ServeResult {
        let h = sha256_hex(input_json);
        let mut consumed = self.consumed.lock().unwrap();
        for (i, cell) in self.cells.iter().enumerate() {
            if consumed[i] || cell.op != op {
                continue;
            }
            if cell.input_hash == h || cell.input_hash_redacted.as_deref() == Some(h.as_str()) {
                if cell.truncated {
                    return ServeResult::Truncated { index: i };
                }
                consumed[i] = true;
                return ServeResult::Served {
                    index: i,
                    outcome: OpOutcome {
                        denied: cell.denied,
                        timing: None,
                        content: cell.content.clone(),
                        view: cell.view.clone(),
                        is_error: cell.is_error,
                    },
                };
            }
        }
        ServeResult::Miss
    }

    /// Overwrite the divergence latch unconditionally — used for a truncated match, which always
    /// reports the exact cell that blocked replay even if an earlier miss already latched a
    /// different (less specific) message.
    fn latch(&self, msg: String) {
        *self.diverged.lock().unwrap() = Some(msg);
    }

    /// Latch only if nothing has latched yet — used for a plain miss, so `diverged()` keeps
    /// reporting the FIRST divergence (the most useful one for diagnosis) rather than the last.
    fn latch_first(&self, msg: String) {
        let mut d = self.diverged.lock().unwrap();
        if d.is_none() {
            *d = Some(msg);
        }
    }

    /// Serve the recorded outcome for `(op, input)` — the out-of-order-tolerant matcher. `None`
    /// means divergence (already latched with a diagnostic).
    pub fn serve(&self, op: &str, input_json: &str) -> Option<OpOutcome> {
        match self.serve_result(op, input_json) {
            ServeResult::Served { outcome, .. } => Some(outcome),
            ServeResult::Truncated { index } => {
                self.latch(truncated_divergence(op, index));
                None
            }
            ServeResult::Miss => {
                let h = sha256_hex(input_json);
                self.latch_first(format!(
                    "op `{op}` (input hash {}…) has no matching unconsumed recorded cell — the \
                     plan's dataflow diverged from the recording",
                    &h[..16.min(h.len())]
                ));
                None
            }
        }
    }

    /// The same dual-hash matcher as [`serve`](Self::serve), but a **plain** miss returns `None`
    /// WITHOUT latching divergence — for a caller that falls through to a live dispatch on a miss
    /// (`Frozen`'s live-bridge, `Resume`'s tail), a miss is not itself a divergence. A truncated
    /// match still latches loudly: the recorded side effect already fired, so re-serving stale bytes
    /// or re-firing it live are both unsafe — that stays a hard, latched refusal in every mode.
    pub fn serve_nonlatching(&self, op: &str, input_json: &str) -> Option<OpOutcome> {
        match self.serve_result(op, input_json) {
            ServeResult::Served { outcome, .. } => Some(outcome),
            ServeResult::Truncated { index } => {
                self.latch(truncated_divergence(op, index));
                None
            }
            ServeResult::Miss => None,
        }
    }
}

/// D-181: the turn-scoped run-trace window — `RunEvent`s belonging strictly to `turn_id`'s span
/// (from its own `TurnStarted`, whose `global_seq` **is** `turn_id`, up to but excluding the next
/// turn's `TurnStarted`, or the end of the stream for the session's most recent turn).
///
/// `RunEvent`s carry no `turn_id` of their own — [`EventStore::record_run_event`] never tags one
/// (wiring that through the live dispatch/write path is a `FlowStore`/engine change, out of this
/// crate's read-side fix) — so [`EventStore::load_turn`]'s `turn_id`-column filter can't be used
/// here; it would return only the turn's `TurnStarted` anchor and none of its run events. This
/// achieves the same partition positionally, using only the already-public read API
/// ([`EventStore::turns`] for the turn boundaries, [`EventStore::load_stream`] for the events),
/// which is exactly what a whole-session [`EventStore::run_trace`] fold was missing: without it, a
/// later turn that re-accepts an identical (same [`flux_lang::runtime::flow_key`]) plan inherits an
/// earlier turn's completed statements and cassette cells — see `resurrect.rs`'s and `whatif.rs`'s
/// module docs.
pub(crate) fn turn_run_trace(
    events: &EventStore,
    session: &str,
    turn_id: i64,
) -> crate::Result<Vec<RunEvent>> {
    let turns = events.turns(session)?;
    let upper = turns
        .iter()
        .map(|t| t.turn_id)
        .filter(|&id| id > turn_id)
        .min();
    let window: Vec<_> = events
        .load_stream(session, None)?
        .into_iter()
        .filter(|e| e.global_seq >= turn_id && upper.is_none_or(|u| e.global_seq < u))
        .collect();
    Ok(flux_events::run_trace(&window))
}

/// [`turn_run_trace`] concatenated over several turns, in ascending `turn_id` order — turns never
/// interleave within one stream, so this is exactly the trace a contiguous multi-turn prefix would
/// have produced, without pulling in anything from a turn NOT in `turn_ids` (unlike filtering a
/// whole-session trace by content-addressed `flow_key` set membership, which a repeated identical
/// plan can leak through — D-181).
pub(crate) fn turns_run_trace(
    events: &EventStore,
    session: &str,
    turn_ids: &[i64],
) -> crate::Result<Vec<RunEvent>> {
    let mut out = Vec::new();
    for &turn_id in turn_ids {
        out.extend(turn_run_trace(events, session, turn_id)?);
    }
    Ok(out)
}

/// The truncated-cell divergence message — worded once, shared by every scope that refuses to serve
/// a capped cell (`ReplayTape`, `FrozenTape`, `ResumeTape`).
fn truncated_divergence(op: &str, index: usize) -> String {
    format!(
        "recorded cell {index} for op `{op}` was truncated at the capture cap — this run is not \
         hermetically replayable past it (re-record with a larger FLUX_CASSETTE_MAX_BYTES)"
    )
}

/// The result of matching one dispatch against a tape's unconsumed cells — the shared core
/// [`ReplayTape::serve`] and [`ReplayTape::serve_nonlatching`] both build on (and that `FrozenTape`/
/// `ResumeTape` reuse via their inner `ReplayTape`), so the dual-hash matcher exists exactly once.
enum ServeResult {
    /// A live, unconsumed cell matched by `(op, hash)` — already marked consumed.
    Served { index: usize, outcome: OpOutcome },
    /// A matching cell exists but was capped at capture time — never served.
    Truncated { index: usize },
    /// No unconsumed cell matches `(op, hash)`.
    Miss,
}

/// What a [`FrozenTape`] or [`ResumeTape`] dispatch resolved to — consumed by the ONE dispatch
/// chokepoint in `runtime.rs`'s `ExecutorHost::dispatch`. Distinguishes a scope-local refusal
/// (latched, shaped into an in-band error, never a silent continuation) from a pass-through miss
/// (falls into the SAME live-dispatch path every other scope uses — no second dispatch site).
pub enum ScopeServe {
    /// Served from tape (substituted or recorded) — the live executor is never touched.
    Served(OpOutcome),
    /// This scope refuses to serve (and refuses to fall through) — carries the human-readable
    /// divergence/refusal reason.
    Refused(String),
    /// No matching cell, and this scope allows falling through to a live dispatch.
    Miss,
}

/// `Frozen` mode (D-175): serve every op from a recorded [`ReplayTape`] — the byte-frozen world —
/// with optional per-op/per-cell substitutions applied before falling back to the recorded outcome.
/// A miss either halts (hermetic, [`OffTape::Halt`]) or bridges to a live [`RecordScope`]
/// ([`OffTape::Live`]). Powers Tune (`Session::what_if`, D-176) and the Test Kit's world-pinned
/// re-plan (`Scenario::check`, D-174).
#[non_exhaustive]
pub struct FrozenTape {
    tape: ReplayTape,
    off_tape: OffTape,
    /// Substitute a recorded op's outcome by op name — checked AFTER `subs_by_cell`, so pinning one
    /// exact call site always wins over a blanket by-op substitution.
    subs_by_op: HashMap<String, OpOutcome>,
    /// Substitute by the recorded cell's index in the tape (the caller maps a target node to a cell
    /// index at construction time; this type only indexes on it).
    subs_by_cell: HashMap<usize, OpOutcome>,
    /// The live-bridge target when `off_tape == Live`; always `None` under `Halt`.
    bridge: Option<RecordScope>,
    went_live: AtomicUsize,
    substituted: AtomicUsize,
    /// D-177: when set, the Frozen dispatch arm re-authorizes a served cell against the CURRENT
    /// executor's policy before serving it. Inert until D-177 wires the check — the field, its
    /// accessor, and this builder exist now so wiring it later is additive, not another breaking
    /// `CassetteScope` change.
    reauthorize: bool,
    policy_denials: Mutex<Vec<String>>,
}

impl FrozenTape {
    /// Hermetic: a miss halts and latches — the frozen world is the ENTIRE world. Tune's default and
    /// the Test Kit's `check()`. Build `tape` with [`ReplayTape::from_trace`] or
    /// [`ReplayTape::from_cells`].
    pub fn hermetic(tape: ReplayTape) -> Self {
        Self {
            tape,
            off_tape: OffTape::Halt,
            subs_by_op: HashMap::new(),
            subs_by_cell: HashMap::new(),
            bridge: None,
            went_live: AtomicUsize::new(0),
            substituted: AtomicUsize::new(0),
            reauthorize: false,
            policy_denials: Mutex::new(Vec::new()),
        }
    }

    /// Live-bridge: a miss falls through to `bridge`'s live `RecordScope` and is recorded into its
    /// tail — the frozen world is a PREFIX a re-plan may legitimately read past.
    pub fn live_bridge(tape: ReplayTape, bridge: RecordScope) -> Self {
        Self {
            tape,
            off_tape: OffTape::Live,
            subs_by_op: HashMap::new(),
            subs_by_cell: HashMap::new(),
            bridge: Some(bridge),
            went_live: AtomicUsize::new(0),
            substituted: AtomicUsize::new(0),
            reauthorize: false,
            policy_denials: Mutex::new(Vec::new()),
        }
    }

    /// Substitute the outcome of every recorded call to `op` (unless a more specific
    /// [`substitute_cell`](Self::substitute_cell) also matches).
    pub fn substitute_op(mut self, op: impl Into<String>, outcome: OpOutcome) -> Self {
        self.subs_by_op.insert(op.into(), outcome);
        self
    }

    /// Substitute the outcome of the recorded cell at `index` specifically (wins over
    /// [`substitute_op`](Self::substitute_op)).
    pub fn substitute_cell(mut self, index: usize, outcome: OpOutcome) -> Self {
        self.subs_by_cell.insert(index, outcome);
        self
    }

    /// D-177: opt this scope into re-authorization of served cells. Default `false` (inert).
    pub fn with_reauthorize(mut self, on: bool) -> Self {
        self.reauthorize = on;
        self
    }

    /// Whether re-authorization is enabled (D-177 reads this before serving a matched cell).
    pub fn reauthorize(&self) -> bool {
        self.reauthorize
    }

    /// Which mode a miss falls into.
    pub fn off_tape(&self) -> OffTape {
        self.off_tape
    }

    /// How many dispatches fell through to a live bridge dispatch.
    pub fn went_live(&self) -> usize {
        self.went_live.load(Ordering::SeqCst)
    }

    /// How many served cells were substituted rather than served as recorded.
    pub fn substituted(&self) -> usize {
        self.substituted.load(Ordering::SeqCst)
    }

    /// The first latched divergence, if any.
    pub fn diverged(&self) -> Option<String> {
        self.tape.diverged()
    }

    /// Unconsumed cells remaining on the inner tape.
    pub fn remaining(&self) -> usize {
        self.tape.remaining()
    }

    /// Record a policy denial (D-177: a stricter policy refusing a served cell) so `hermetic()`
    /// reflects it — a denial must surface as a real divergence, never be masked by tape service.
    pub fn note_policy_denial(&self, reason: impl Into<String>) {
        self.policy_denials.lock().unwrap().push(reason.into());
    }

    /// Every policy denial recorded so far.
    pub fn policy_denials(&self) -> Vec<String> {
        self.policy_denials.lock().unwrap().clone()
    }

    /// True iff this run never touched live IO, never latched a divergence, and never hit a policy
    /// denial — the honesty gate `Counterfactual`/`Report` surfaces (D-176/D-174) rely on to decide
    /// whether a diff is complete or must localize a divergence instead.
    pub fn is_hermetic(&self) -> bool {
        self.went_live() == 0 && self.diverged().is_none() && self.policy_denials().is_empty()
    }

    /// Serve one dispatch: a matched cell is substituted (by cell index, then by op name) if a
    /// substitution was registered, else served as recorded. A truncated match NEVER falls through
    /// to live in either mode (the recorded side effect already fired) — always `Refused`. A miss is
    /// `OffTape::Halt`'s latch-and-refuse or `OffTape::Live`'s pass-through `Miss`.
    pub fn serve(&self, op: &str, input_json: &str) -> ScopeServe {
        match self.tape.serve_result(op, input_json) {
            ServeResult::Served { index, outcome } => {
                let served = if let Some(sub) = self.subs_by_cell.get(&index) {
                    self.substituted.fetch_add(1, Ordering::SeqCst);
                    sub.clone()
                } else if let Some(sub) = self.subs_by_op.get(op) {
                    self.substituted.fetch_add(1, Ordering::SeqCst);
                    sub.clone()
                } else {
                    outcome
                };
                ScopeServe::Served(served)
            }
            ServeResult::Truncated { index } => {
                let msg = truncated_divergence(op, index);
                self.tape.latch(msg.clone());
                ScopeServe::Refused(msg)
            }
            ServeResult::Miss => match self.off_tape {
                OffTape::Halt => {
                    let msg = format!(
                        "op `{op}` has no matching unconsumed recorded cell and this Frozen scope \
                         is `OffTape::Halt` — the frozen world is the whole world here, so a miss \
                         is a divergence, not a cue to run live"
                    );
                    self.tape.latch_first(msg.clone());
                    ScopeServe::Refused(msg)
                }
                OffTape::Live => ScopeServe::Miss,
            },
        }
    }

    /// Record one live-bridge dispatch's outcome into the tail (`OffTape::Live` only — the runtime
    /// only calls this after a `Miss`, which only `Live` mode ever returns).
    pub fn record_tail(&self, redactor: &Redactor, op: &str, input_json: &str, out: &OpOutcome) {
        if let Some(bridge) = &self.bridge {
            bridge.record(redactor, op, input_json, out);
        }
        self.went_live.fetch_add(1, Ordering::SeqCst);
    }
}

/// `Resume` mode (D-175): serve *completed* ops from the crash-tail cell slice — exactly-once, since
/// a served cell is consumed like [`ReplayTape`]'s. A miss falls through to a live dispatch recorded
/// into `tail` for the ledger; a truncated matched cell refuses rather than re-firing a side effect
/// whose bytes were capped at capture. Powers Resurrect (D-178).
#[non_exhaustive]
pub struct ResumeTape {
    tape: ReplayTape,
    tail: RecordScope,
    served: AtomicUsize,
    ran_live: AtomicUsize,
}

impl ResumeTape {
    /// `cells` is the crash-tail slice (driver-built — `OpRecorded` cells strictly after the last
    /// completed statement), `tail` is where a live fallback dispatch gets recorded.
    pub fn new(cells: Vec<Cell>, tail: RecordScope) -> Self {
        Self {
            tape: ReplayTape::from_cells(cells),
            tail,
            served: AtomicUsize::new(0),
            ran_live: AtomicUsize::new(0),
        }
    }

    /// Serve one dispatch: a hit is served exactly-once (consumed, never re-fired); a truncated
    /// match refuses rather than re-firing a side effect whose recorded bytes were capped; a plain
    /// miss is `Miss` and NEVER latches — an unrecorded op is the expected, documented at-least-once
    /// window (the op fired before its cell was appended), not a divergence.
    pub fn serve(&self, op: &str, input_json: &str) -> ScopeServe {
        match self.tape.serve_result(op, input_json) {
            ServeResult::Served { outcome, .. } => {
                self.served.fetch_add(1, Ordering::SeqCst);
                ScopeServe::Served(outcome)
            }
            ServeResult::Truncated { index } => {
                let msg = truncated_divergence(op, index);
                self.tape.latch(msg.clone());
                ScopeServe::Refused(msg)
            }
            ServeResult::Miss => ScopeServe::Miss,
        }
    }

    /// Record one live-tail dispatch's outcome — called after a `Miss` falls through to the real
    /// executor, so the ledger sees the crash tail's new cells exactly like any other recording.
    pub fn record_tail(&self, redactor: &Redactor, op: &str, input_json: &str, out: &OpOutcome) {
        self.tail.record(redactor, op, input_json, out);
        self.ran_live.fetch_add(1, Ordering::SeqCst);
    }

    /// How many cells were served exactly-once from the crash-tail slice.
    pub fn served(&self) -> usize {
        self.served.load(Ordering::SeqCst)
    }

    /// How many ops fell through and ran live (the honest at-least-once window).
    pub fn ran_live(&self) -> usize {
        self.ran_live.load(Ordering::SeqCst)
    }

    /// Unconsumed cells remaining on the crash-tail slice (a clean resume ends at 0).
    pub fn remaining(&self) -> usize {
        self.tape.remaining()
    }

    /// The first latched divergence, if any (a truncated served cell — never a plain miss).
    pub fn diverged(&self) -> Option<String> {
        self.tape.diverged()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorded_input_view_is_redacted_capped_and_backward_compatible() {
        let events = Arc::new(EventStore::in_memory().unwrap());
        let session = events.create_session("mock").unwrap();
        let scope = RecordScope {
            events: events.clone(),
            session: session.clone(),
            seq: AtomicU32::new(0),
            cap: 24,
        };
        let redactor = Redactor::new();
        redactor.add_secret("SUPERSECRET");
        scope.record(
            &redactor,
            "echo",
            r#"{"token":"SUPERSECRET","tail":"abcdefghijklmnopqrstuvwxyz"}"#,
            &OpOutcome::ok("done"),
        );

        let trace = events.run_trace(&session).unwrap();
        match &trace[0] {
            RunEvent::OpRecorded {
                input_view,
                input_view_truncated,
                truncated,
                ..
            } => {
                let input = input_view.as_deref().expect("presentation input recorded");
                assert!(!input.contains("SUPERSECRET"));
                assert!(input.contains("[redacted]"));
                assert!(*input_view_truncated);
                assert!(!truncated, "input truncation does not poison replayability");
            }
            other => panic!("wrong event: {other:?}"),
        }
    }

    #[test]
    fn recorded_input_view_redacts_json_escaped_secrets() {
        let events = Arc::new(EventStore::in_memory().unwrap());
        let session = events.create_session("mock").unwrap();
        let scope = RecordScope {
            events: events.clone(),
            session: session.clone(),
            seq: AtomicU32::new(0),
            cap: 1024,
        };
        let secret = "quote\" backslash\\ embedded\nnewline secret";
        let redactor = Redactor::new();
        redactor.add_secret(secret);
        let input = serde_json::to_string(&serde_json::json!({ "token": secret })).unwrap();

        scope.record(&redactor, "echo", &input, &OpOutcome::ok("done"));

        let trace = events.run_trace(&session).unwrap();
        let RunEvent::OpRecorded {
            input_view: Some(input_view),
            redacted,
            ..
        } = &trace[0]
        else {
            panic!("expected a recorded input view");
        };
        let decoded: serde_json::Value = serde_json::from_str(input_view).unwrap();
        assert_eq!(decoded["token"], "[redacted]");
        assert!(*redacted, "structured input redaction marks the cell");
    }

    /// C-323 — `input_view` is durable, so a node kind this walker skipped is a permanent leak. An
    /// all-digit credential has no protection but registration, so a `Value::Number` it never
    /// descended into was exactly such a hole; an object *key* was the other.
    ///
    /// The view must also stay **parseable** (the TUI re-parses it) and must not over-redact: an
    /// unregistered number keeps its value and its type.
    #[test]
    fn recorded_input_view_redacts_a_registered_numeric_credential_and_keeps_parsing() {
        const NUMERIC: &str = "216216216216216218";
        let redactor = Redactor::new();
        redactor.add_secret(NUMERIC);
        let input = serde_json::to_string(&serde_json::json!({
            "account_id": 216_216_216_216_216_218_i64,
            NUMERIC: "as-a-key",
            "port": 8080,
            "ok": true,
        }))
        .unwrap();

        let (view, changed) = redacted_input_view(&redactor, &input);

        assert!(changed, "a redacted view marks the cell");
        assert!(
            !view.contains(NUMERIC),
            "a registered numeric credential is durable in the view: {view}"
        );
        let decoded: serde_json::Value =
            serde_json::from_str(&view).expect("the view stays parseable JSON: {view}");
        assert_eq!(decoded["account_id"], "[redacted]");
        assert!(decoded.get("[redacted]").is_some(), "key not scrubbed");
        assert_eq!(decoded["port"], 8080);
        assert!(decoded["port"].is_number(), "port retyped: {view}");
        assert_eq!(decoded["ok"], true);
    }

    /// An input carrying nothing registered is returned **byte-identical** and unflagged — the
    /// re-encoding path C-323 introduced must not touch a view that needed no redaction.
    #[test]
    fn an_unredacted_input_view_is_returned_verbatim() {
        let redactor = Redactor::new();
        redactor.add_secret("some-unrelated-secret");
        let input = r#"{"port":8080,"secret_ttl":3600,"path":"note.txt"}"#;
        assert_eq!(
            redacted_input_view(&redactor, input),
            (input.to_string(), false)
        );
    }

    fn cell(op: &str, input: &str, content: &str) -> Cell {
        Cell {
            op: op.into(),
            input_hash: sha256_hex(input),
            input_hash_redacted: None,
            content: content.into(),
            view: None,
            is_error: false,
            denied: false,
            truncated: false,
        }
    }

    /// Sequential serves degenerate to the strict cursor.
    #[test]
    fn tape_serves_in_order_for_sequential_dispatches() {
        let tape = ReplayTape::from_cells(vec![
            cell("read", "{\"a\":1}", "one"),
            cell("read", "{\"a\":2}", "two"),
        ]);
        assert_eq!(tape.serve("read", "{\"a\":1}").unwrap().content, "one");
        assert_eq!(tape.serve("read", "{\"a\":2}").unwrap().content, "two");
        assert_eq!(tape.remaining(), 0);
        assert!(tape.diverged().is_none());
    }

    /// The matcher absorbs a `parallel`-shaped interleaving swap: cells recorded in one order are
    /// served green when the replayed dispatch order differs.
    #[test]
    fn tape_absorbs_out_of_order_parallel_interleaving() {
        let tape = ReplayTape::from_cells(vec![
            cell("read", "{\"b\":1}", "B"),
            cell("read", "{\"a\":1}", "A"),
        ]);
        assert_eq!(tape.serve("read", "{\"a\":1}").unwrap().content, "A");
        assert_eq!(tape.serve("read", "{\"b\":1}").unwrap().content, "B");
        assert!(tape.diverged().is_none());
    }

    /// A redaction-shifted input matches via `input_hash_redacted`.
    #[test]
    fn tape_matches_redaction_shifted_input_via_redacted_hash() {
        let raw = "{\"text\":\"token SECRETVAL end\"}";
        let red = "{\"text\":\"token [redacted] end\"}";
        let mut c = cell("fmt", raw, "ok");
        c.input_hash_redacted = Some(sha256_hex(red));
        let tape = ReplayTape::from_cells(vec![c]);
        assert_eq!(tape.serve("fmt", red).unwrap().content, "ok");
    }

    /// No matching cell → divergence latched, never silent.
    #[test]
    fn tape_divergence_is_latched_loudly() {
        let tape = ReplayTape::from_cells(vec![cell("read", "{\"a\":1}", "one")]);
        assert!(tape.serve("read", "{\"a\":999}").is_none());
        assert!(tape
            .diverged()
            .unwrap()
            .contains("no matching unconsumed recorded cell"));
    }

    /// A truncated cell refuses hermetic service.
    #[test]
    fn tape_refuses_truncated_cells() {
        let mut c = cell("read", "{\"a\":1}", "partial");
        c.truncated = true;
        let tape = ReplayTape::from_cells(vec![c]);
        assert!(tape.serve("read", "{\"a\":1}").is_none());
        assert!(tape.diverged().unwrap().contains("truncated"));
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        let (t, tr) = truncate_chars("héllo", 2);
        assert!(tr);
        assert_eq!(t, "h"); // é is 2 bytes starting at 1; boundary walk lands at 1
    }

    // ---- D-175: serve_nonlatching ----

    #[test]
    fn serve_nonlatching_plain_miss_returns_none_without_latching() {
        let tape = ReplayTape::from_cells(vec![cell("read", "{\"a\":1}", "one")]);
        assert!(tape.serve_nonlatching("read", "{\"a\":999}").is_none());
        assert!(
            tape.diverged().is_none(),
            "a plain miss must not latch — the caller is expected to fall through to live"
        );
    }

    #[test]
    fn serve_nonlatching_truncated_cell_still_latches_loudly() {
        let mut c = cell("read", "{\"a\":1}", "partial");
        c.truncated = true;
        let tape = ReplayTape::from_cells(vec![c]);
        assert!(tape.serve_nonlatching("read", "{\"a\":1}").is_none());
        assert!(
            tape.diverged().unwrap().contains("truncated"),
            "a truncated match is a hard refusal in every mode, latched exactly like `serve`"
        );
    }

    #[test]
    fn serve_nonlatching_hit_consumes_exactly_like_serve() {
        let tape = ReplayTape::from_cells(vec![cell("read", "{\"a\":1}", "one")]);
        assert_eq!(
            tape.serve_nonlatching("read", "{\"a\":1}").unwrap().content,
            "one"
        );
        assert_eq!(tape.remaining(), 0);
        // The cell was consumed — a second serve is a miss (still non-latching).
        assert!(tape.serve_nonlatching("read", "{\"a\":1}").is_none());
        assert!(tape.diverged().is_none());
    }

    // ---- D-175: FrozenTape ----

    #[test]
    fn frozen_substitute_by_cell_index_wins_over_substitute_by_op() {
        let tape = ReplayTape::from_cells(vec![cell("read", "{\"a\":1}", "recorded")]);
        let frozen = FrozenTape::hermetic(tape)
            .substitute_op("read", OpOutcome::ok("by-op"))
            .substitute_cell(0, OpOutcome::ok("by-cell"));
        match frozen.serve("read", "{\"a\":1}") {
            ScopeServe::Served(out) => assert_eq!(out.content, "by-cell"),
            _ => panic!("expected Served"),
        }
        assert_eq!(frozen.substituted(), 1);
    }

    #[test]
    fn frozen_substitute_by_op_applies_when_no_cell_override() {
        let tape = ReplayTape::from_cells(vec![cell("read", "{\"a\":1}", "recorded")]);
        let frozen = FrozenTape::hermetic(tape).substitute_op("read", OpOutcome::ok("by-op"));
        match frozen.serve("read", "{\"a\":1}") {
            ScopeServe::Served(out) => assert_eq!(out.content, "by-op"),
            _ => panic!("expected Served"),
        }
        assert_eq!(frozen.substituted(), 1);
    }

    #[test]
    fn frozen_serves_the_recorded_outcome_when_unsubstituted() {
        let tape = ReplayTape::from_cells(vec![cell("read", "{\"a\":1}", "recorded")]);
        let frozen = FrozenTape::hermetic(tape);
        match frozen.serve("read", "{\"a\":1}") {
            ScopeServe::Served(out) => assert_eq!(out.content, "recorded"),
            _ => panic!("expected Served"),
        }
        assert_eq!(frozen.substituted(), 0);
        assert!(frozen.is_hermetic());
    }

    #[test]
    fn frozen_halt_miss_latches_and_refuses() {
        let frozen = FrozenTape::hermetic(ReplayTape::from_cells(vec![]));
        match frozen.serve("read", "{\"a\":1}") {
            ScopeServe::Refused(msg) => assert!(msg.contains("OffTape::Halt")),
            _ => panic!("expected Refused"),
        }
        assert!(frozen.diverged().is_some());
        assert!(!frozen.is_hermetic());
    }

    #[test]
    fn frozen_live_miss_is_a_passthrough_not_a_divergence() {
        let root = std::env::temp_dir().join(format!("flux-cassette-frozen-live-{}", line!()));
        std::fs::create_dir_all(&root).ok();
        let events = Arc::new(EventStore::in_memory().unwrap());
        let session = events.create_session("mock").unwrap();
        let bridge = RecordScope::new(events.clone(), session);
        let frozen = FrozenTape::live_bridge(ReplayTape::from_cells(vec![]), bridge);
        match frozen.serve("read", "{\"a\":1}") {
            ScopeServe::Miss => {}
            _ => panic!("expected Miss — a live-bridge miss is not itself a divergence"),
        }
        assert!(frozen.diverged().is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn frozen_truncated_cell_never_falls_through_to_live_even_in_live_mode() {
        let events = Arc::new(EventStore::in_memory().unwrap());
        let session = events.create_session("mock").unwrap();
        let bridge = RecordScope::new(events.clone(), session);
        let mut c = cell("read", "{\"a\":1}", "partial");
        c.truncated = true;
        let frozen = FrozenTape::live_bridge(ReplayTape::from_cells(vec![c]), bridge);
        match frozen.serve("read", "{\"a\":1}") {
            ScopeServe::Refused(msg) => assert!(msg.contains("truncated")),
            _ => panic!(
                "a truncated matched cell must refuse, never fall through to live — the recorded \
                 side effect already fired"
            ),
        }
        assert_eq!(frozen.went_live(), 0);
    }

    #[test]
    fn frozen_hermetic_reflects_live_and_denial_state() {
        let events = Arc::new(EventStore::in_memory().unwrap());
        let session = events.create_session("mock").unwrap();
        let bridge = RecordScope::new(events.clone(), session);
        let frozen = FrozenTape::live_bridge(ReplayTape::from_cells(vec![]), bridge);
        assert!(frozen.is_hermetic(), "no dispatch has happened yet");
        frozen.record_tail(&Redactor::new(), "read", "{}", &OpOutcome::ok("live"));
        assert_eq!(frozen.went_live(), 1);
        assert!(!frozen.is_hermetic(), "went_live must break hermeticity");

        let clean = FrozenTape::hermetic(ReplayTape::from_cells(vec![]));
        clean.note_policy_denial("D-177 policy floor refused it");
        assert!(
            !clean.is_hermetic(),
            "a policy denial must break hermeticity too"
        );
        assert_eq!(clean.policy_denials().len(), 1);
    }

    #[test]
    fn frozen_reauthorize_defaults_off_and_the_builder_flips_it() {
        let frozen = FrozenTape::hermetic(ReplayTape::from_cells(vec![]));
        assert!(!frozen.reauthorize(), "inert until D-177");
        let frozen = frozen.with_reauthorize(true);
        assert!(frozen.reauthorize());
    }

    // ---- D-175: ResumeTape ----

    #[test]
    fn resume_serves_a_hit_exactly_once_then_misses() {
        let events = Arc::new(EventStore::in_memory().unwrap());
        let session = events.create_session("mock").unwrap();
        let tail = RecordScope::new(events.clone(), session);
        let resume = ResumeTape::new(vec![cell("read", "{\"a\":1}", "one")], tail);
        match resume.serve("read", "{\"a\":1}") {
            ScopeServe::Served(out) => assert_eq!(out.content, "one"),
            _ => panic!("expected Served"),
        }
        assert_eq!(resume.served(), 1);
        assert_eq!(resume.remaining(), 0);
        // Exactly-once: the same call now misses (never re-served).
        match resume.serve("read", "{\"a\":1}") {
            ScopeServe::Miss => {}
            _ => panic!("expected Miss on the second call — exactly-once service"),
        }
        assert!(resume.diverged().is_none(), "a plain miss never latches");
    }

    #[test]
    fn resume_truncated_cell_refuses_without_refiring() {
        let events = Arc::new(EventStore::in_memory().unwrap());
        let session = events.create_session("mock").unwrap();
        let tail = RecordScope::new(events.clone(), session);
        let mut c = cell("read", "{\"a\":1}", "partial");
        c.truncated = true;
        let resume = ResumeTape::new(vec![c], tail);
        match resume.serve("read", "{\"a\":1}") {
            ScopeServe::Refused(msg) => assert!(msg.contains("truncated")),
            _ => panic!("expected Refused"),
        }
        assert_eq!(resume.served(), 0);
        assert!(resume.diverged().is_some());
    }

    #[test]
    fn resume_plain_miss_never_latches_the_honest_at_least_once_window() {
        let events = Arc::new(EventStore::in_memory().unwrap());
        let session = events.create_session("mock").unwrap();
        let tail = RecordScope::new(events.clone(), session);
        let resume = ResumeTape::new(vec![], tail);
        match resume.serve("read", "{\"a\":1}") {
            ScopeServe::Miss => {}
            _ => panic!("expected Miss"),
        }
        assert!(
            resume.diverged().is_none(),
            "an unrecorded op is the documented at-least-once window, not a divergence"
        );
        resume.record_tail(
            &Redactor::new(),
            "read",
            "{\"a\":1}",
            &OpOutcome::ok("live"),
        );
        assert_eq!(resume.ran_live(), 1);
    }
}
