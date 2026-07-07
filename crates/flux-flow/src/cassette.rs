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

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use flux_events::{EventStore, NewEvent};
use flux_lang::ast::{RunEvent, StepId};
use flux_lang::host::OpOutcome;
use flux_lang::runtime::sha256_hex;
use flux_secret::Redactor;

/// Ops never cassetted: the loop-machinery pair is host-internal (hidden from model plans; their
/// "outputs" are whole transcripts) — recording them would bloat the tape with cells replay never
/// consumes, and serving them from tape would be meaningless (the replay driver re-executes inner
/// plans directly and never re-drives the outer loop).
const SKIP_OPS: &[&str] = &["plan", "run_plan"];

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

/// The active cassette mode, installed on the `FlowStore` by the engine (record, every turn), the
/// replay driver (replay), or the fork engine (replay for the prefix, then record for the tail).
pub enum CassetteScope {
    Record(RecordScope),
    Replay(ReplayTape),
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
        if SKIP_OPS.contains(&op) {
            return;
        }
        let input_hash = sha256_hex(input_json);
        let red_input = redactor.redact(input_json);
        let input_hash_redacted = (red_input != input_json).then(|| sha256_hex(&red_input));

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
            redacted: content_redacted || v_redacted || input_hash_redacted.is_some(),
            input_hash_redacted,
            content,
            view,
            is_error: outcome.is_error,
            denied: outcome.denied,
            truncated: c_trunc || v_trunc,
        };
        let _ = self.events.append(&self.session, NewEvent::run(cell));
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
        let cells: Vec<Cell> = trace
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
            .collect();
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

    /// Serve the recorded outcome for `(op, input)` — the out-of-order-tolerant matcher. `None`
    /// means divergence (already latched with a diagnostic).
    pub fn serve(&self, op: &str, input_json: &str) -> Option<OpOutcome> {
        let h = sha256_hex(input_json);
        let mut consumed = self.consumed.lock().unwrap();
        for (i, cell) in self.cells.iter().enumerate() {
            if consumed[i] || cell.op != op {
                continue;
            }
            if cell.input_hash == h || cell.input_hash_redacted.as_deref() == Some(h.as_str()) {
                if cell.truncated {
                    let msg = format!(
                        "recorded cell {i} for op `{op}` was truncated at the capture cap — this \
                         run is not hermetically replayable past it (re-record with a larger \
                         FLUX_CASSETTE_MAX_BYTES)"
                    );
                    *self.diverged.lock().unwrap() = Some(msg);
                    return None;
                }
                consumed[i] = true;
                return Some(OpOutcome {
                    denied: cell.denied,
                    content: cell.content.clone(),
                    view: cell.view.clone(),
                    is_error: cell.is_error,
                });
            }
        }
        let msg = format!(
            "op `{op}` (input hash {}…) has no matching unconsumed recorded cell — the plan's \
             dataflow diverged from the recording",
            &h[..16.min(h.len())]
        );
        let mut d = self.diverged.lock().unwrap();
        if d.is_none() {
            *d = Some(msg);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
