//! A-145 — **the durable log → renderable timeline projection**, and the fidelity table that says
//! how much of the screen it can honestly rebuild.
//!
//! A-144's fixture was hand-authored by the same context that then chose a layout from it. This
//! module replaces the load-bearing half of it with a **capture of a real recorded run**: a reduced,
//! scrubbed projection of `~/.flux/events.db`, committed as JSONL and reconstructed here into the
//! same [`Fixture`] the five renderers already consume.
//!
//! # This is C-422's work, started
//!
//! [C-422](../../../../docs/stories/C-422-the-render-projection.md) owns "turn a recorded session
//! into the ordered, timestamped timeline a renderer can paint", and its finding is that the TUI's
//! existing durable→screen path (`crate::projection::historical_observation_entry`) handles five
//! observation kinds against twenty-six live `UiEvent` variants. Rather than grow a second,
//! private reconstruction for the mocks, the reconstruction lives **here, in one place**, with
//! [`FIDELITY`] as the beginning of C-422's fidelity table. What this module cannot rebuild is a
//! finding about the *log*, not a gap in the mocks — see [`Fidelity::Absent`].
//!
//! # The three things the log makes hard
//!
//! 1. **Observations are batch-flushed at the turn watermark.** Every `observation` row in a turn
//!    carries the *flush* timestamp, not the moment it describes: in the captured session, 250
//!    observations spanning 33 minutes all land within 100 ms of `turn_ended`. `ts` paces `run`
//!    events faithfully and paces observations not at all. The reconstruction therefore takes its
//!    whole spine from `run` events and treats observations as unordered attributes.
//! 2. **Run events carry no `turn_id`.** Only `plan_attempted`/`turn_ended`/`call_usage` are
//!    turn-scoped; a step is attributed to a turn by falling between its `turn_started` and
//!    `turn_ended` in `global_seq` order.
//! 3. **The log has no future and no present.** Every recorded step is finished, so a replay of a
//!    finished session has no running step — and the question a live loop view exists to answer is
//!    "what is it doing *right now*". The reconstruction therefore replays to a [`Cursor`] and
//!    draws whatever is still open at it as [`Status::Running`]; `Status::Pending` has no durable
//!    source at all (an adaptive run authors no plan beyond its next op). Both are marked in
//!    [`FIDELITY`], and — the C-422 discipline — the cursor is **visible in every render's header**
//!    rather than only in this comment.
//!
//! # ⚠ The capture is a snapshot, and cannot be refreshed
//!
//! `captures/*.jsonl` came off one machine at one moment. Re-running the capture command produces a
//! *different* run; nothing regenerates this file. The command is recorded in the story so the
//! **method** is reproducible even though the run is not.

use serde_json::{Map, Value};

use super::fixture::{Fixture, Provenance, Status, Step, StepKind};

/// The captured run the real load cases are reconstructed from. See `captures/` for the file and
/// `crates/flux-tui/examples/capture_run.rs` for the command that produced it.
pub const CAPTURE_JSONL: &str = include_str!("captures/s_1477-docs-and-release.jsonl");

/// How well one concept of the live screen survives a round trip through the durable log. This is
/// the vocabulary [C-422](../../../../docs/stories/C-422-the-render-projection.md)'s fidelity table
/// is owed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fidelity {
    /// Rebuilt from a durable event, with no invention.
    Faithful,
    /// Synthesised from something adjacent. The render must say so.
    Approximated,
    /// Not recorded. The render shows nothing rather than a guess.
    Absent,
}

impl Fidelity {
    pub fn label(self) -> &'static str {
        match self {
            Fidelity::Faithful => "faithful",
            Fidelity::Approximated => "approximated",
            Fidelity::Absent => "absent",
        }
    }
}

/// One row of the fidelity table: a thing the loop view wants to draw, and what the log can say
/// about it.
#[derive(Debug, Clone, Copy)]
pub struct FidelityRow {
    /// What the view wants.
    pub concept: &'static str,
    /// The durable source, or `"—"` when there is none.
    pub source: &'static str,
    pub fidelity: Fidelity,
    /// Why — and, for an approximation, exactly what was invented.
    pub note: &'static str,
}

/// **The deliverable.** Every element of the loop view this reconstruction had to produce, and how.
///
/// Scoped to what these five layouts draw — it is not yet the per-`UiEvent`-variant table C-422
/// owes, but every row here is one C-422 does not have to rediscover. Rows are ordered
/// faithful → approximated → absent so the honest part of the picture is not buried.
pub const FIDELITY: &[FidelityRow] = &[
    FidelityRow {
        concept: "step identity, op name, nesting",
        source: "RunEvent::StepStarted / StepSucceeded / StepFailed",
        fidelity: Fidelity::Faithful,
        note: "the bracketing of started/succeeded reconstructs the tree exactly; the capture \
               closes all 182 of its steps with no orphan and no unmatched close",
    },
    FidelityRow {
        concept: "step start, end and duration",
        source: "the events' own `ts` (ms)",
        fidelity: Fidelity::Faithful,
        note: "`run` rows are written live, so their ms timestamps are the real pacing — 3 ms to \
               234 s in this capture, five orders of magnitude",
    },
    FidelityRow {
        concept: "tool input",
        source: "RunEvent::OpRecorded.input_view",
        fidelity: Fidelity::Faithful,
        note: "redacted and bounded at record time (C-43); the capture keeps its first 100 chars",
    },
    FidelityRow {
        concept: "tool output",
        source: "RunEvent::OpRecorded.content + is_error/denied/truncated",
        fidelity: Fidelity::Faithful,
        note: "durably redacted since C-43. The capture keeps the first line and the true byte \
               length, so `44.9k` in a detail pane is measured, not styled",
    },
    FidelityRow {
        concept: "failure",
        source: "RunEvent::StepFailed.error, OpRecorded.is_error",
        fidelity: Fidelity::Faithful,
        note: "both the flow-level failure and the op-level error survive with their message",
    },
    FidelityRow {
        concept: "operator approval pause",
        source: "the `approve_batch` step's own duration",
        fidelity: Fidelity::Faithful,
        note: "the pause is a bracketed step, so its wall time is exact — 39.7 s, 7.9 s, 5.8 s and \
               4.7 s here. `approval.requested`/`approved` observations add the tool name but are \
               batch-flushed, so nothing is taken from them",
    },
    FidelityRow {
        concept: "the turn, its prompt and its answer",
        source: "EventKind::TurnStarted / TurnEnded",
        fidelity: Fidelity::Faithful,
        note: "run events carry no turn_id, so a step joins a turn by global_seq order between the \
               two — ordering the store already guarantees",
    },
    FidelityRow {
        concept: "the authored plan",
        source: "EventKind::PlanAttempted.plan_source",
        fidelity: Fidelity::Approximated,
        note: "SYNTHESISED: which plan is 'current'. The run emits one accepted plan per op — 127 \
               of them, every single one a one-op flow — so there is no per-turn DAG to show. The \
               view takes the most recent accepted plan at the cursor and renders its `plan_source` \
               as text; `plan_ast` is never persisted, so the syntax highlighting the live plan \
               card gets is lost",
    },
    FidelityRow {
        concept: "`Running` status / what it is doing now",
        source: "steps still open at the replay cursor",
        fidelity: Fidelity::Approximated,
        note: "SYNTHESISED: the cursor. Every recorded step is finished, so a finished session has \
               no present tense; the reconstruction replays to one instant and draws what is open \
               at it. The header says `@ <cursor>` in every render",
    },
    FidelityRow {
        concept: "token cost of a model call",
        source: "EventKind::CallUsage",
        fidelity: Fidelity::Approximated,
        note: "SYNTHESISED: the attribution. CallUsage rows are flushed in a block at turn end in \
               provider-call order, with no step id and no usable ts, so they are assigned to a \
               turn's model steps in order. The totals are real; which call got which slice is not",
    },
    FidelityRow {
        concept: "loop phase name (`orient` / `gather` / `execute`)",
        source: "observation `loop.phase`, `PlanAttempted.phase`",
        fidelity: Fidelity::Approximated,
        note: "SYNTHESISED: the position. `loop.phase` is batch-flushed and carries no anchor, so \
               the phase is read off the top-level op instead (`explore` is a gather bracket, \
               `execute_batch` an execute bracket) — right for this loop, and a guess for any other",
    },
    FidelityRow {
        concept: "`Pending` — steps not yet run",
        source: "—",
        fidelity: Fidelity::Absent,
        note: "the log has no future. An adaptive run authors no plan beyond its next op, so there \
               is nothing to grey out. A-144's hand-authored fixture drew pending phases; the real \
               cases draw none, and the difference is the log's, not the layout's",
    },
    FidelityRow {
        concept: "streaming tail / partial output (`ToolProgress`)",
        source: "—",
        fidelity: Fidelity::Absent,
        note: "C-158's live tail is never persisted; only the finished `content` is. A detail pane \
               replaying a capture can show the result and never the progress",
    },
    FidelityRow {
        concept: "time-to-first-token, chunk pacing",
        source: "observation `model.call` (ttft_us, chunks)",
        fidelity: Fidelity::Absent,
        note: "recorded as totals, but the observation is batch-flushed and unanchored, so it can \
               time no particular call on the timeline. Present in the log, unusable for pacing",
    },
    FidelityRow {
        concept: "sub-agent activity (`SpawnActivity`)",
        source: "observation `subagent.trace`, child streams via `children_of`",
        fidelity: Fidelity::Absent,
        note: "⚠ absent FROM THIS CAPTURE, and structurally hard: no run in the store that has \
               `op_recorded` (post-C-43) also has sub-agents. The 12 sub-agent streams present are \
               all pre-C-43, so a real fan-out and a real op output cannot currently be shown by \
               the same capture. This is why the fan-out and deep-nesting cases stay synthetic",
    },
    FidelityRow {
        concept: "compaction (`Compacted`)",
        source: "—",
        fidelity: Fidelity::Absent,
        note: "the store holds 112 114 events and not one `compacted` row, so the shape C-422 must \
               decide (pre- or post-compaction) has no fixture anywhere in this log to decide it \
               against",
    },
    FidelityRow {
        concept: "the operator's own identity",
        source: "observation `turn.identity`, `tool_call.caller`",
        fidelity: Fidelity::Absent,
        note: "recorded — 192 rows carry the local username — and deliberately DROPPED at capture. \
               Nothing about the loop view needs it and it is not publishable",
    },
];

// ---------------------------------------------------------------------------
// The capture file
// ---------------------------------------------------------------------------

/// One line of a capture: a durable event reduced to the fields this projection reads.
///
/// Deliberately **not** `flux_events::EventKind`. A capture is a publishable artifact, so it is a
/// narrow allow-list of fields rather than a serialization of whatever the log happens to hold —
/// `turn.identity`'s `caller`, the toolchain listing that names locally installed plugins, and the
/// bodies of every observation are dropped at capture rather than trimmed afterwards.
///
/// The JSON mapping below is written out by hand rather than derived. That is not an accident of
/// what `flux-tui` happens to depend on: an allow-list whose every field is named in code, in one
/// place, on both the read and the write side, is the artifact a redaction review can actually be
/// held to. `#[derive(Serialize)]` on a struct is a promise that whatever the struct grows later
/// gets published too.
#[derive(Debug, Clone)]
pub struct CaptureEvent {
    /// `global_seq` — the log's total order, and the only ordering this projection trusts.
    pub s: u64,
    /// `ts` in ms. Meaningful for `run` rows; see the module docs for why not for observations.
    pub t: i64,
    pub body: Body,
}

/// The reduced event bodies a capture carries.
///
/// `crates/flux-tui/examples/capture_run.rs` writes exactly this type through [`CaptureEvent::to_json`]
/// and this module reads it back through [`CaptureEvent::from_json`], so producer and consumer
/// cannot drift apart.
#[derive(Debug, Clone)]
pub enum Body {
    /// The header line: what was captured, from where, and with what scrubbed.
    Capture {
        session: String,
        title: String,
        command: String,
        scrubbed: Vec<String>,
    },
    SessionStarted {
        model: String,
    },
    TurnStarted {
        input: String,
    },
    TurnEnded {
        outcome: String,
        answer: String,
    },
    StepStarted {
        id: String,
        op: String,
    },
    StepOk {
        id: String,
    },
    StepFailed {
        id: String,
        err: String,
    },
    /// `RunEvent::OpRecorded`, trimmed to a display-sized head.
    Op {
        id: String,
        input: String,
        out: String,
        /// True byte length of the recorded output, before the capture trimmed it.
        n: u64,
        is_error: bool,
        denied: bool,
        truncated: bool,
    },
    /// An accepted `PlanAttempted`, `plan_source` trimmed.
    Plan {
        src: String,
    },
    /// One `CallUsage`. See [`FIDELITY`] for why its position is an approximation.
    Usage {
        u: [u64; 3],
    },
}

impl CaptureEvent {
    /// The one JSON line this event is written as. Every published field is named here.
    pub fn to_json(&self) -> Value {
        let mut obj = Map::new();
        obj.insert("s".into(), self.s.into());
        obj.insert("t".into(), self.t.into());
        let mut put = |k: &str, v: Value| {
            obj.insert(k.to_string(), v);
        };
        match &self.body {
            Body::Capture {
                session,
                title,
                command,
                scrubbed,
            } => {
                put("k", "capture".into());
                put("session", session.as_str().into());
                put("title", title.as_str().into());
                put("command", command.as_str().into());
                put("scrubbed", scrubbed.clone().into());
            }
            Body::SessionStarted { model } => {
                put("k", "session_started".into());
                put("model", model.as_str().into());
            }
            Body::TurnStarted { input } => {
                put("k", "turn_started".into());
                put("in", input.as_str().into());
            }
            Body::TurnEnded { outcome, answer } => {
                put("k", "turn_ended".into());
                put("outcome", outcome.as_str().into());
                put("answer", answer.as_str().into());
            }
            Body::StepStarted { id, op } => {
                put("k", "step_started".into());
                put("id", id.as_str().into());
                put("op", op.as_str().into());
            }
            Body::StepOk { id } => {
                put("k", "step_ok".into());
                put("id", id.as_str().into());
            }
            Body::StepFailed { id, err } => {
                put("k", "step_failed".into());
                put("id", id.as_str().into());
                put("err", err.as_str().into());
            }
            Body::Op {
                id,
                input,
                out,
                n,
                is_error,
                denied,
                truncated,
            } => {
                put("k", "op".into());
                put("id", id.as_str().into());
                put("in", input.as_str().into());
                put("out", out.as_str().into());
                put("n", (*n).into());
                for (key, flag) in [
                    ("is_error", *is_error),
                    ("denied", *denied),
                    ("truncated", *truncated),
                ] {
                    if flag {
                        put(key, true.into());
                    }
                }
            }
            Body::Plan { src } => {
                put("k", "plan".into());
                put("src", src.as_str().into());
            }
            Body::Usage { u } => {
                put("k", "usage".into());
                put("u", vec![u[0], u[1], u[2]].into());
            }
        }
        Value::Object(obj)
    }

    /// Read one capture line back. `None` for a line whose `k` this build does not know — a capture
    /// written by a newer command degrades to fewer steps rather than failing to load.
    fn from_json(v: &Value) -> Option<Self> {
        let s = v.get("s").and_then(Value::as_u64).unwrap_or(0);
        let t = v.get("t").and_then(Value::as_i64).unwrap_or(0);
        let text = |k: &str| {
            v.get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let flag = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
        let body = match v.get("k").and_then(Value::as_str)? {
            "capture" => Body::Capture {
                session: text("session"),
                title: text("title"),
                command: text("command"),
                scrubbed: v
                    .get("scrubbed")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect(),
            },
            "session_started" => Body::SessionStarted {
                model: text("model"),
            },
            "turn_started" => Body::TurnStarted { input: text("in") },
            "turn_ended" => Body::TurnEnded {
                outcome: text("outcome"),
                answer: text("answer"),
            },
            "step_started" => Body::StepStarted {
                id: text("id"),
                op: text("op"),
            },
            "step_ok" => Body::StepOk { id: text("id") },
            "step_failed" => Body::StepFailed {
                id: text("id"),
                err: text("err"),
            },
            "op" => Body::Op {
                id: text("id"),
                input: text("in"),
                out: text("out"),
                n: v.get("n").and_then(Value::as_u64).unwrap_or(0),
                is_error: flag("is_error"),
                denied: flag("denied"),
                truncated: flag("truncated"),
            },
            "plan" => Body::Plan { src: text("src") },
            "usage" => {
                let u = v.get("u").and_then(Value::as_array)?;
                let at = |i: usize| u.get(i).and_then(Value::as_u64).unwrap_or(0);
                Body::Usage {
                    u: [at(0), at(1), at(2)],
                }
            }
            _ => return None,
        };
        Some(CaptureEvent { s, t, body })
    }
}

/// A parsed capture: the header plus its events, in `global_seq` order.
#[derive(Debug, Clone)]
pub struct Capture {
    pub session: String,
    pub title: String,
    pub command: String,
    /// What the capture command removed beyond what the `Redactor` did.
    pub scrubbed: Vec<String>,
    pub events: Vec<CaptureEvent>,
}

/// Parse a capture file. Panics on a malformed line — a capture is committed data, so a bad line is
/// a broken artifact rather than a runtime condition.
pub fn parse(jsonl: &str) -> Capture {
    let mut events = Vec::new();
    let mut header = None;
    for (i, line) in jsonl.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("capture line {}: {e}", i + 1));
        let Some(event) = CaptureEvent::from_json(&value) else {
            continue;
        };
        match event.body {
            Body::Capture {
                ref session,
                ref title,
                ref command,
                ref scrubbed,
            } => {
                header = Some((
                    session.clone(),
                    title.clone(),
                    command.clone(),
                    scrubbed.clone(),
                ))
            }
            _ => events.push(event),
        }
    }
    let (session, title, command, scrubbed) = header.expect("capture has no header line");
    Capture {
        session,
        title,
        command,
        scrubbed,
        events,
    }
}

// ---------------------------------------------------------------------------
// The projection
// ---------------------------------------------------------------------------

/// Which slice of a capture to reconstruct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slice {
    /// Every turn.
    WholeSession,
    /// One 1-based turn.
    Turn(usize),
}

/// The instant the view is drawn at.
///
/// A finished session has no present tense (see the module docs), so one has to be chosen. The rule
/// is deterministic and stated rather than tuned: **halfway through the slice's longest-running
/// step**. That is the moment a live view is under the most pressure — a long bracket open, its
/// finished children behind it — which is the moment the layouts have to be compared at.
fn cursor_ms(steps: &[Step]) -> u64 {
    fn longest(steps: &[Step], best: &mut (u64, u64)) {
        for step in steps {
            if step.dur_ms > best.0 {
                *best = (step.dur_ms, step.start_ms + step.dur_ms / 2);
            }
            longest(&step.children, best);
        }
    }
    let mut best = (0u64, 0u64);
    longest(steps, &mut best);
    best.1
}

/// Reconstruct a [`Fixture`] from a capture.
///
/// The spine is the bracketing of `step_started` against `step_ok`/`step_failed`; everything else
/// decorates it. See [`FIDELITY`] for what each field of the result is worth.
pub fn reconstruct(capture: &Capture, slice: Slice) -> Fixture {
    let mut roots: Vec<Step> = Vec::new();
    // Open brackets, innermost last. Index 0 is always the turn the steps belong to.
    let mut open: Vec<Step> = Vec::new();
    let mut turn_no = 0usize;
    let mut in_slice = false;
    let mut model = String::new();
    let mut plan = String::new();
    let mut plan_at = 0u64;
    let mut usage: Vec<[u64; 3]> = Vec::new();
    let mut origin: Option<i64> = None;

    // Close the innermost bracket whose id matches, dropping anything opened inside it that never
    // closed — the log's own nesting, not a heuristic. `now` is an offset from the slice's origin,
    // the same units `Step::start_ms` is in.
    fn close(open: &mut Vec<Step>, roots: &mut Vec<Step>, id: &str, now: u64, status: Status) {
        let Some(at) = open.iter().rposition(|s| s.trace_id == id) else {
            return;
        };
        while open.len() > at + 1 {
            let child = open.pop().expect("non-empty");
            attach(open, roots, child);
        }
        let mut step = open.pop().expect("non-empty");
        step.status = status;
        step.dur_ms = now.saturating_sub(step.start_ms);
        attach(open, roots, step);
    }

    fn attach(open: &mut [Step], roots: &mut Vec<Step>, step: Step) {
        match open.last_mut() {
            Some(parent) => parent.children.push(step),
            None => roots.push(step),
        }
    }

    for event in &capture.events {
        // Everything downstream works in ms since the slice's first turn started, which is what
        // `Step::start_ms` means. Mixing that with the log's absolute `ts` is the one arithmetic
        // mistake this projection can make.
        let now = |origin: Option<i64>| (event.t - origin.unwrap_or(event.t)).max(0) as u64;
        match &event.body {
            Body::SessionStarted { model: m } => model = m.clone(),
            Body::TurnStarted { input } => {
                turn_no += 1;
                in_slice = match slice {
                    Slice::WholeSession => true,
                    Slice::Turn(n) => turn_no == n,
                };
                if !in_slice {
                    continue;
                }
                origin.get_or_insert(event.t);
                usage.clear();
                open.push(turn_step(turn_no, input, now(origin)));
            }
            Body::TurnEnded { outcome, answer } => {
                if !in_slice {
                    continue;
                }
                while open.len() > 1 {
                    let child = open.pop().expect("non-empty");
                    attach(&mut open, &mut roots, child);
                }
                if let Some(mut turn) = open.pop() {
                    turn.status = if outcome == "ok" {
                        Status::Done
                    } else {
                        Status::Failed
                    };
                    turn.dur_ms = now(origin).saturating_sub(turn.start_ms);
                    turn.note = trim(answer, 3);
                    spend(&mut turn, &usage);
                    roots.push(turn);
                }
            }
            Body::StepStarted { id, op } => {
                if !in_slice {
                    continue;
                }
                open.push(op_step(id, op, now(origin)));
            }
            Body::StepOk { id } => {
                if in_slice {
                    close(&mut open, &mut roots, id, now(origin), Status::Done);
                }
            }
            Body::StepFailed { id, err } => {
                if !in_slice {
                    continue;
                }
                if let Some(step) = open.iter_mut().rev().find(|s| s.trace_id == *id) {
                    step.note = err.clone();
                }
                close(&mut open, &mut roots, id, now(origin), Status::Failed);
            }
            Body::Op {
                id,
                input,
                out,
                n,
                is_error,
                denied,
                truncated,
            } => {
                if !in_slice {
                    continue;
                }
                let Some(step) = open.iter_mut().rev().find(|s| s.trace_id == *id) else {
                    continue;
                };
                step.detail = summarize_input(input);
                step.note = output_note(out, *n, *truncated);
                if *is_error {
                    step.status = Status::Failed;
                }
                if *denied {
                    step.detail = format!("denied · {}", step.detail);
                }
            }
            Body::Plan { src } => {
                if in_slice {
                    plan = src.clone();
                    plan_at = now(origin);
                }
            }
            Body::Usage { u } => {
                if in_slice {
                    usage.push(*u);
                }
            }
            // The header is consumed by `parse` and never reaches the event list.
            Body::Capture { .. } => {}
        }
    }
    // Anything still open at the end of the capture stays open (a cancelled or crashed run).
    while let Some(step) = open.pop() {
        attach(&mut open, &mut roots, step);
    }

    let cursor = cursor_ms(&roots);
    apply_cursor(&mut roots, cursor);

    let title = format!("{} · {}", capture.title, capture.session);
    Fixture::recorded(
        &title,
        cursor,
        roots,
        plan_payload(&plan, plan_at),
        Provenance::Recorded {
            session: capture.session.clone(),
            model,
            cursor_ms: cursor,
        },
    )
}

/// The turn bracket every step of a turn hangs under. Run events carry no `turn_id`, so this is the
/// structure `global_seq` order implies rather than one the log states.
fn turn_step(n: usize, input: &str, start: u64) -> Step {
    let mut step = Step::at(
        StepKind::Phase,
        &format!("turn {n}"),
        &trim(input, 1),
        Status::Running,
        start,
        0,
    );
    step.trace_id = format!("__turn{n}");
    step
}

/// One recorded step. The kind comes from the op name — see [`FIDELITY`]'s "loop phase name" row
/// for why that is a reading of *this* loop rather than a durable fact.
fn op_step(id: &str, op: &str, start: u64) -> Step {
    let kind = match op {
        // The bounded semantic slot, twice: the intent classifier and the answer writer.
        "detect_intent" | "present_results" => StepKind::Model,
        // Brackets, not work: each contains the ops the loop decided to run in that phase.
        "explore" | "execute_batch" | "approve_batch" => StepKind::Phase,
        "task" => StepKind::Spawn,
        _ => StepKind::Tool,
    };
    let detail = match op {
        "approve_batch" => format!("{} operator approval", super::PAUSE_GLYPH),
        _ => String::new(),
    };
    let mut step = Step::at(kind, op, &detail, Status::Running, start, 0);
    step.trace_id = id.to_string();
    step
}

/// Attribute a turn's `CallUsage` rows to its model steps, in order. Approximated — see
/// [`FIDELITY`].
fn spend(turn: &mut Step, usage: &[[u64; 3]]) {
    fn walk(step: &mut Step, usage: &[[u64; 3]], next: &mut usize) {
        if step.kind == StepKind::Model {
            if let Some(u) = usage.get(*next) {
                step.usage = Some(super::Usage {
                    input: u[0],
                    output: u[1],
                    cached: u[2],
                });
                *next += 1;
            }
        }
        for child in &mut step.children {
            walk(child, usage, next);
        }
    }
    let mut next = 0usize;
    for child in &mut turn.children {
        walk(child, usage, &mut next);
    }
}

/// Replay to the cursor: a step that had not started yet is dropped, a step still open at it is
/// [`Status::Running`] with its elapsed-so-far. This is the one place the reconstruction invents
/// anything structural, and [`Provenance::Recorded::cursor_ms`] puts it on screen.
fn apply_cursor(steps: &mut Vec<Step>, cursor: u64) {
    steps.retain(|s| s.start_ms <= cursor);
    for step in steps.iter_mut() {
        if step.start_ms + step.dur_ms > cursor {
            step.status = Status::Running;
            step.dur_ms = cursor - step.start_ms;
        }
        apply_cursor(&mut step.children, cursor);
    }
}

/// A `flow.plan`-shaped payload for `crate::plan::render`. `plan_ast` is never persisted, so this
/// takes the `plan` string fallback — the durable `plan_source` — and the render is plain text.
fn plan_payload(src: &str, at: u64) -> serde_json::Value {
    // The capture flattens `plan_source` onto one line, and this loop authors exactly one op per
    // plan — hence `1`, not a count of anything. See [`FIDELITY`]'s "the authored plan" row.
    serde_json::json!({
        "risk": format!("accepted +{}s", at / 1000),
        "ops": usize::from(!src.is_empty()),
        "plan": src,
    })
}

/// The most informative thing in a recorded `input_view`, for a one-line row. Falls back to the
/// raw (already redacted, already bounded) JSON.
fn summarize_input(input: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(input) else {
        return trim(input, 1);
    };
    for key in ["path", "pattern", "query", "glob", "label", "message"] {
        if let Some(s) = value.get(key).and_then(|v| v.as_str()) {
            if !s.is_empty() {
                return trim(s, 1);
            }
        }
    }
    if let Some(paths) = value.get("paths").and_then(|v| v.as_array()) {
        let first = paths.first().and_then(|v| v.as_str()).unwrap_or("");
        return match paths.len() {
            0 | 1 => trim(first, 1),
            n => format!("{} +{}", trim(first, 1), n - 1),
        };
    }
    trim(input, 1)
}

/// A recorded output as a detail-pane body: the head the capture kept, and the size it was cut
/// from. The measured size is the point — an elision the operator can see is honest, one they
/// cannot is the bug this whole module is about.
fn output_note(out: &str, bytes: u64, truncated: bool) -> String {
    let size = if bytes >= 1000 {
        format!("{:.1}k", bytes as f64 / 1000.0)
    } else {
        format!("{bytes}")
    };
    let mark = if truncated { " · capped at record" } else { "" };
    if out.is_empty() {
        return format!("{size} bytes{mark}");
    }
    format!("{out}\n… {size} bytes recorded{mark}")
}

/// First `lines` lines, whitespace-collapsed.
fn trim(s: &str, lines: usize) -> String {
    s.lines()
        .take(lines)
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_committed_capture_parses_and_declares_its_own_provenance() {
        let capture = parse(CAPTURE_JSONL);
        assert_eq!(capture.session, "s_1477");
        assert!(
            capture.command.contains("capture_run"),
            "the capture does not record the command that made it: {:?}",
            capture.command,
        );
        assert!(
            !capture.scrubbed.is_empty(),
            "a capture that scrubbed nothing has not been reviewed",
        );
    }

    #[test]
    fn every_step_the_log_opened_is_closed_by_the_log_itself() {
        // The reconstruction's one structural claim. If bracketing ever needed a heuristic, this is
        // where it would show up as a step whose duration came from nowhere.
        let capture = parse(CAPTURE_JSONL);
        let opened = capture
            .events
            .iter()
            .filter(|e| matches!(e.body, Body::StepStarted { .. }))
            .count();
        let closed = capture
            .events
            .iter()
            .filter(|e| matches!(e.body, Body::StepOk { .. } | Body::StepFailed { .. }))
            .count();
        assert_eq!(
            opened, closed,
            "{opened} steps opened, {closed} closed — the capture is truncated mid-run",
        );
    }

    #[test]
    fn the_cursor_leaves_something_running() {
        // Without this the whole comparison is invalid: a finished session drawn as finished has no
        // "what is it doing right now", which is the only question a live loop view exists for.
        for slice in [Slice::WholeSession, Slice::Turn(7)] {
            let fx = reconstruct(&parse(CAPTURE_JSONL), slice);
            let running = fx
                .flatten()
                .iter()
                .filter(|f| f.step.status == Status::Running)
                .count();
            assert!(running > 0, "{slice:?}: nothing is in flight at the cursor");
        }
    }

    #[test]
    fn the_fidelity_table_covers_the_three_classes_and_explains_every_approximation() {
        for row in FIDELITY {
            assert!(!row.note.is_empty(), "{}: no reason given", row.concept);
            if row.fidelity == Fidelity::Approximated {
                assert!(
                    row.note.contains("SYNTHESISED"),
                    "{}: approximated without naming what was invented",
                    row.concept,
                );
            }
            if row.fidelity == Fidelity::Absent {
                assert_eq!(
                    row.source, "—",
                    "{}: absent but names a durable source",
                    row.concept,
                );
            }
        }
        for class in [
            Fidelity::Faithful,
            Fidelity::Approximated,
            Fidelity::Absent,
        ] {
            assert!(
                FIDELITY.iter().any(|r| r.fidelity == class),
                "the table has no {} row — that is a table that agrees with itself",
                class.label(),
            );
        }
    }
}
