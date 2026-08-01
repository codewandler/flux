//! The four load cases the five mocks draw — **two reconstructed from a recorded run, two still
//! hand-authored** — and the vocabulary they share.
//!
//! # Which is which, and why it matters (A-145)
//!
//! A-144 authored all four by hand, and its hard cases (49 steps, eight levels, six workers) were
//! *chosen* by the same context that then picked a layout from them. A-145 replaces the two that
//! carry the comparison with a projection of a real session out of `~/.flux/events.db`
//! ([`super::capture`]):
//!
//! | case | provenance | what it is |
//! |---|---|---|
//! | [`LoadCase::Tidy`] | **recorded** | one turn of `s_1477` — "now, commit all docs in a smart way" |
//! | [`LoadCase::LongRun`] | **recorded** | all nine turns of `s_1477`, replayed to a cursor |
//! | [`LoadCase::DeepNesting`] | hand-authored | seven levels of nested delegation |
//! | [`LoadCase::FanOut`] | hand-authored | six sub-agents running at once |
//!
//! ⚠ The last two stay synthetic because **the log cannot currently produce them**: no session in
//! the store that records op output (post-C-43) also spawns sub-agents, so a real fan-out and a
//! real tool result cannot come from one capture. That is a finding about the log, recorded in
//! [`super::capture::FIDELITY`], not a shortcut here — and [`Provenance`] is drawn in every render
//! header so no reader has to remember which case they are looking at.
//!
//! Every step maps onto a shape `UiEvent` already carries
//! (`crates/flux-tui/src/controller.rs`): [`StepKind::Plan`] is `Plan`, [`StepKind::Phase`] is
//! `Phase`, [`StepKind::Tool`] is `ToolCall`/`ToolTiming`/`ToolResult`, [`StepKind::Model`] is
//! `Intent` + `CallUsage`, [`StepKind::Spawn`] is `SpawnActivity`. Nothing here invents a new
//! event.

use serde_json::{json, Value};

use super::capture::{self, Slice};

/// The four fixtures. The tidy one is what a mock flatters itself with; the other three are the
/// ones that decide whether it is a candidate.
pub const LOAD_CASES: [LoadCase; 4] = [
    LoadCase::Tidy,
    LoadCase::LongRun,
    LoadCase::DeepNesting,
    LoadCase::FanOut,
];

/// Which shape of run to draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadCase {
    /// **Recorded.** One turn of the captured session: a documentation commit, mid-flight.
    Tidy,
    /// **Recorded.** The whole captured session — nine turns, 33 minutes, replayed to a cursor.
    LongRun,
    /// **Hand-authored.** Nested delegation: a sub-agent that spawns a sub-agent, seven levels down.
    DeepNesting,
    /// **Hand-authored.** Six tracker-audit workers running at once, each mid-op.
    FanOut,
}

impl LoadCase {
    /// The case's name as the snapshot set and the example label it.
    pub fn name(self) -> &'static str {
        match self {
            LoadCase::Tidy => "tidy",
            LoadCase::LongRun => "long run",
            LoadCase::DeepNesting => "deep nesting",
            LoadCase::FanOut => "fan-out",
        }
    }

    /// Whether this case comes off a real machine or out of somebody's head. Drawn, not just
    /// documented — see [`Provenance`].
    pub fn is_recorded(self) -> bool {
        matches!(self, LoadCase::Tidy | LoadCase::LongRun)
    }
}

/// Where a fixture's data came from. Carried on [`Fixture`] and rendered into every mock's header,
/// because a comparison that silently mixes measured and invented load is worse than either.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Provenance {
    /// Reconstructed from a committed capture of a real session.
    Recorded {
        /// The session id in the store it came from.
        session: String,
        /// The model that ran it.
        model: String,
        /// Offset from run start of the instant the view is drawn at. The one structural
        /// approximation the projection makes; see [`super::capture`].
        cursor_ms: u64,
    },
    /// Written by hand for layout pressure. The timings are plausible, not measured.
    HandAuthored,
}

impl Provenance {
    /// The badge every header carries.
    pub fn badge(&self) -> String {
        match self {
            Provenance::Recorded {
                session, cursor_ms, ..
            } => format!("recorded {session} @ +{}s", cursor_ms / 1000),
            Provenance::HandAuthored => "hand-authored".to_string(),
        }
    }
}

/// What a step is, from the closed set `UiEvent` already carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
    /// `UiEvent::Plan` — the authored DAG the runtime froze.
    Plan,
    /// `UiEvent::Phase` — a named bracket in the loop.
    Phase,
    /// `UiEvent::ToolCall` + `ToolTiming` + `ToolResult`.
    Tool,
    /// `UiEvent::Intent` + `UiEvent::CallUsage` — the bounded semantic slot.
    Model,
    /// `UiEvent::SpawnActivity` — one sub-agent.
    Spawn,
}

impl StepKind {
    /// The kind's one-character sigil. Kept distinct from the status glyph: a mock has to show
    /// *what* a step is as well as how it went, and the mocks differ in whether they can afford
    /// both columns.
    pub fn sigil(self) -> &'static str {
        match self {
            StepKind::Plan => "▤",
            StepKind::Phase => "§",
            StepKind::Tool => "→",
            StepKind::Model => "✻",
            StepKind::Spawn => "⇩",
        }
    }
}

/// How a step went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Done,
    Running,
    Pending,
    Failed,
}

impl Status {
    pub fn glyph(self) -> &'static str {
        match self {
            Status::Done => "✓",
            Status::Running => "▶",
            Status::Pending => "·",
            Status::Failed => "✗",
        }
    }
}

/// What one model call cost — `UiEvent::CallUsage`'s payload, trimmed to what a row can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cached: u64,
}

/// One node of the run. The five renderers differ in which fields they can afford to show, which
/// is most of what the comparison is about.
#[derive(Debug, Clone)]
pub struct Step {
    /// Pre-order index, assigned by [`Fixture::new`]. Stable within a fixture.
    pub id: usize,
    /// The durable `StepId` this step was reconstructed from, empty for hand-authored steps. Used
    /// only while bracketing a capture — no renderer reads it.
    pub trace_id: String,
    pub kind: StepKind,
    /// The short name — an op, a phase, a role.
    pub label: String,
    /// The one-line summary a condensed row shows.
    pub detail: String,
    /// The longer body only a detail pane has room for.
    pub note: String,
    pub status: Status,
    /// Offset from run start.
    pub start_ms: u64,
    /// Elapsed, or elapsed-so-far for a running step.
    pub dur_ms: u64,
    pub usage: Option<Usage>,
    pub children: Vec<Step>,
}

impl Step {
    fn new(
        kind: StepKind,
        label: &str,
        detail: &str,
        status: Status,
        start: u64,
        dur: u64,
    ) -> Self {
        Self {
            id: 0,
            trace_id: String::new(),
            kind,
            label: label.to_string(),
            detail: detail.to_string(),
            note: String::new(),
            status,
            start_ms: start,
            dur_ms: dur,
            usage: None,
            children: Vec::new(),
        }
    }

    /// [`Step::new`], reachable from [`super::capture`] — the reconstruction builds the same steps
    /// the hand-authored cases do, so the five renderers cannot tell the two apart.
    pub(super) fn at(
        kind: StepKind,
        label: &str,
        detail: &str,
        status: Status,
        start: u64,
        dur: u64,
    ) -> Self {
        Self::new(kind, label, detail, status, start, dur)
    }

    fn note(mut self, note: &str) -> Self {
        self.note = note.to_string();
        self
    }

    fn usage(mut self, input: u64, output: u64, cached: u64) -> Self {
        self.usage = Some(Usage {
            input,
            output,
            cached,
        });
        self
    }

    fn kids(mut self, children: Vec<Step>) -> Self {
        self.children = children;
        self
    }

    /// This step and every descendant.
    fn count(&self) -> usize {
        1 + self.children.iter().map(Step::count).sum::<usize>()
    }
}

/// One step in pre-order, with the structural context a renderer needs.
#[derive(Debug, Clone, Copy)]
pub struct Flat<'a> {
    pub step: &'a Step,
    pub depth: usize,
    /// Whether this step is its parent's last child — the `└─` vs `├─` decision.
    pub last: bool,
    /// Bit `k` is set when this step's ancestor at depth `k` was itself a last child, so a tree
    /// renderer knows to leave that guide column blank instead of continuing a `│` past a subtree
    /// that has ended. A bitmask rather than an owned prefix string keeps [`Flat`] `Copy`, which
    /// is what lets the windowing helper slice it without allocating.
    pub trail: u64,
}

impl Flat<'_> {
    /// The guide columns that come before this step's own connector — three cells per ancestor.
    pub fn guides(&self) -> String {
        (1..self.depth)
            .map(|k| {
                if self.trail & (1u64 << k.min(63)) != 0 {
                    "   "
                } else {
                    "│  "
                }
            })
            .collect()
    }
}

/// One run, as five layouts see it.
#[derive(Debug, Clone)]
pub struct Fixture {
    pub title: String,
    /// Wall clock since the run started.
    pub elapsed_ms: u64,
    pub steps: Vec<Step>,
    /// The `flow.plan` observation body the graph mock feeds straight to [`crate::plan::render`] —
    /// the same payload shape the live TUI already receives.
    pub plan: Value,
    /// Measured or invented. Drawn in every header; see [`Provenance`].
    pub provenance: Provenance,
}

impl Fixture {
    fn new(title: &str, elapsed_ms: u64, steps: Vec<Step>) -> Self {
        Self::assembled(
            title,
            elapsed_ms,
            steps,
            plan_observation(),
            Provenance::HandAuthored,
        )
    }

    /// A fixture reconstructed from a capture. Only [`super::capture`] calls this.
    pub(super) fn recorded(
        title: &str,
        elapsed_ms: u64,
        steps: Vec<Step>,
        plan: Value,
        provenance: Provenance,
    ) -> Self {
        Self::assembled(title, elapsed_ms, steps, plan, provenance)
    }

    fn assembled(
        title: &str,
        elapsed_ms: u64,
        mut steps: Vec<Step>,
        plan: Value,
        provenance: Provenance,
    ) -> Self {
        let mut next = 0usize;
        fn assign(steps: &mut [Step], next: &mut usize) {
            for step in steps {
                step.id = *next;
                *next += 1;
                assign(&mut step.children, next);
            }
        }
        assign(&mut steps, &mut next);
        Self {
            title: title.to_string(),
            elapsed_ms,
            steps,
            plan,
            provenance,
        }
    }

    /// Total steps, descendants included — the denominator every elision is measured against.
    pub fn step_count(&self) -> usize {
        self.steps.iter().map(Step::count).sum()
    }

    /// Pre-order walk. The order the thread, the tree and the graph gutter all read in.
    pub fn flatten(&self) -> Vec<Flat<'_>> {
        fn walk<'a>(steps: &'a [Step], depth: usize, trail: u64, out: &mut Vec<Flat<'a>>) {
            for (i, step) in steps.iter().enumerate() {
                let last = i + 1 == steps.len();
                out.push(Flat {
                    step,
                    depth,
                    last,
                    trail,
                });
                let child_trail = if last {
                    trail | (1u64 << depth.min(63))
                } else {
                    trail
                };
                walk(&step.children, depth + 1, child_trail, out);
            }
        }
        let mut out = Vec::new();
        walk(&self.steps, 0, 0, &mut out);
        out
    }

    /// The step a live view would have in focus: the deepest running one, else the last finished.
    pub fn focused(&self) -> &Step {
        let flat = self.flatten();
        flat.iter()
            .filter(|f| f.step.status == Status::Running)
            .max_by_key(|f| f.depth)
            .or_else(|| flat.iter().rev().find(|f| f.step.status == Status::Done))
            .map(|f| f.step)
            .unwrap_or(&self.steps[0])
    }

    /// The ancestor chain down to `id`, as labels — a breadcrumb for the layouts that need one.
    pub fn path_to(&self, id: usize) -> Vec<&str> {
        fn walk<'a>(steps: &'a [Step], id: usize, trail: &mut Vec<&'a str>) -> bool {
            for step in steps {
                trail.push(step.label.as_str());
                if step.id == id || walk(&step.children, id, trail) {
                    return true;
                }
                trail.pop();
            }
            false
        }
        let mut trail = Vec::new();
        walk(&self.steps, id, &mut trail);
        trail
    }
}

/// The fixture for one load case.
pub fn fixture(case: LoadCase) -> Fixture {
    match case {
        // ⚠ Recorded. The turn is chosen for size (18 steps, closest to the 15 A-144 drew) and
        // because it is self-contained: status, diff, stage, commit, with an approval in the middle.
        LoadCase::Tidy => capture::reconstruct(recorded_run(), Slice::Turn(7)),
        LoadCase::LongRun => capture::reconstruct(recorded_run(), Slice::WholeSession),
        // Hand-authored — see the module docs for why the log cannot yet produce these two.
        LoadCase::DeepNesting => deep_nesting(),
        LoadCase::FanOut => fan_out(),
    }
}

/// The committed capture, parsed once. Every render of a recorded case reads this.
fn recorded_run() -> &'static capture::Capture {
    static RUN: std::sync::OnceLock<capture::Capture> = std::sync::OnceLock::new();
    RUN.get_or_init(|| capture::parse(capture::CAPTURE_JSONL))
}

/// Nested delegation seven levels down — a tracker-audit worker that spawns its own worker. The
/// case that punishes indentation.
fn deep_nesting() -> Fixture {
    use Status::*;
    use StepKind::*;
    Fixture::new(
        "tracking board sync · nested delegation",
        18_900,
        vec![
            Step::new(Plan, "plan", "low · mutating · 9 ops", Done, 0, 140),
            Step::new(Phase, "audit", "1 worker · nested", Running, 140, 18_760).kids(vec![
                Step::new(Spawn, "tracker-audit#1", "agent-loop-visibility", Running, 200, 18_700).kids(vec![
                    Step::new(Phase, "scan", "epic → stories", Running, 240, 18_650).kids(vec![
                        Step::new(Tool, "glob", "docs/stories/A-*.md", Done, 250, 90),
                        Step::new(Spawn, "story-check#1", "delegated per-story check", Running, 350, 18_540).kids(vec![
                            Step::new(Phase, "read-epic", "A-137", Running, 380, 18_500).kids(vec![
                                Step::new(Tool, "read", "docs/stories/A-137-the-step-thread.md", Done, 390, 12).kids(vec![
                                    Step::new(Tool, "track.frontmatter", "epic: agent-loop-visibility", Done, 402, 6).kids(vec![
                                        Step::new(Tool, "note", "tracker present — no gap", Done, 408, 2),
                                    ]),
                                ]),
                                Step::new(Model, "model.decide", "opus · judge · is the tracker adequate?", Running, 410, 18_460)
                                    .note("stage: judge · round 1\nevidence: A-137 frontmatter + the epic design\nstreaming…")
                                    .usage(6_200, 0, 5_400),
                            ]),
                        ]),
                    ]),
                ]),
            ]),
            Step::new(Phase, "board", "regenerate", Pending, 0, 0),
        ],
    )
}

/// Six tracker-audit workers, five still running — `SpawnActivity` at the width the fleet pane
/// already sees. The case that punishes any layout with one column of attention.
fn fan_out() -> Fixture {
    use Status::*;
    use StepKind::*;
    let epics = [
        ("agent-loop-visibility", "grep", "\"epic:\" in docs/stories"),
        (
            "flux-lang-hardening",
            "read",
            "docs/designs/flux-lang-hardening.md",
        ),
        ("syntax-simplification", "glob", "docs/stories/L-*.md"),
        (
            "plugin-protocol-decoupling",
            "bash",
            "$ git log --oneline -20 -- plugins/",
        ),
        ("run-control", "read", "docs/designs/run-control.md"),
        (
            "interactive-debugger",
            "track.frontmatter",
            "docs/stories/A-14*.md",
        ),
    ];
    let workers: Vec<Step> = epics
        .iter()
        .enumerate()
        .map(|(i, (epic, op, arg))| {
            let start = 4_900 + (i as u64) * 40;
            Step::new(
                Spawn,
                &format!("tracker-audit#{}", i + 1),
                epic,
                if i == 0 {
                    Status::Done
                } else {
                    Status::Running
                },
                start,
                if i == 0 { 3_150 } else { 12_400 - start },
            )
            .note(&format!(
                "epic {epic}\n  {op} {arg}\n  {} so far",
                if i == 0 { "1 gap" } else { "no gap" }
            ))
            .kids(vec![
                Step::new(Tool, "glob", "docs/stories/*.md", Done, start + 20, 70),
                Step::new(
                    Tool,
                    op,
                    arg,
                    if i == 0 {
                        Status::Done
                    } else {
                        Status::Running
                    },
                    start + 90,
                    if i == 0 { 3_040 } else { 12_400 - start - 90 },
                ),
            ])
        })
        .collect();
    Fixture::new(
        "tracking board sync · audit fan-out",
        12_400,
        vec![
            Step::new(Plan, "plan", "low · mutating · 9 ops", Done, 0, 140),
            Step::new(Phase, "judge", "6 epics untracked", Done, 640, 4_260).kids(vec![Step::new(
                Model,
                "model.decide",
                "opus · judge · round 3 · 6 ops",
                Done,
                660,
                4_220,
            )
            .usage(8_420, 780, 6_100)]),
            Step::new(
                Phase,
                "audit",
                "6 workers · 5 running",
                Running,
                4_900,
                7_500,
            )
            .kids(workers),
            Step::new(Phase, "board", "regenerate", Pending, 0, 0),
        ],
    )
}

/// The `flow.plan` observation body — the same payload the live TUI's plan card already receives,
/// authored by hand here. `plan_ast` is what [`crate::plan::render`] prefers, so the graph mock
/// gets real syntax highlighting through `flux_flow::render::render_styled` rather than a second
/// tree renderer.
///
/// The program does not change between load cases; only what is running inside it does. That is
/// itself one of the findings the graph mock exists to surface.
fn plan_observation() -> Value {
    json!({
        "risk": "low · mutating",
        "ops": 9,
        "plan_ast": {
            "name": "tracking_sync",
            "body": [
                {"kind": "seq", "bind": "validated", "body": [
                    {"kind": "bind", "name": "stories", "value":
                        {"kind": "call", "op": "glob", "args": [
                            {"kind": "lit", "value": {"pattern": "docs/stories/*.md"}}]}},
                    {"kind": "bind", "name": "template", "value":
                        {"kind": "call", "op": "read", "args": [
                            {"kind": "lit", "value": {"path": "docs/stories/_TEMPLATE.md"}}]}},
                    {"kind": "bind", "name": "meta", "value":
                        {"kind": "call", "op": "track.frontmatter", "args": [
                            {"kind": "obj", "fields": {"files": {"kind": "var", "name": "stories"}}}]}}
                ]},
                {"kind": "bind", "name": "verdict", "value":
                    {"kind": "call", "op": "model.decide", "args": [
                        {"kind": "lit", "value": {"stage": "judge"}}]}},
                {"kind": "each", "in": {"kind": "var", "name": "verdict"}, "as": "epic",
                 "collect": "audits", "body": [
                    {"kind": "call", "op": "task", "args": [
                        {"kind": "lit", "value": {"role": "tracker-audit"}}]}
                ]},
                {"kind": "call", "op": "write", "args": [
                    {"kind": "lit", "value": {"path": "docs/stories/README.md"}}]},
                {"kind": "call", "op": "edit", "args": [
                    {"kind": "lit", "value": {"path": "CHANGELOG.md"}}]}
            ]
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hand_authored_plan_is_a_payload_the_live_plan_card_can_already_read() {
        // If this stops deserializing, the graph mock is drawing something the real renderer
        // would not — which would make it useless as evidence for A-138.
        let ast = fixture(LoadCase::FanOut).plan["plan_ast"].clone();
        let parsed: flux_flow::ast::DraftAst =
            serde_json::from_value(ast).expect("plan_ast is a DraftAst");
        assert_eq!(parsed.name.as_deref(), Some("tracking_sync"));
        assert_eq!(parsed.body.len(), 5);
    }

    /// ⚠ A-145's first correction to A-144. The hand-authored fixture handed the graph mock a
    /// `plan_ast` because whoever wrote it wanted syntax highlighting. **No run in the store has
    /// one:** `PlanAttempted` persists `plan_source` (canonical text) and this loop never writes
    /// `plan_text` at all, so a recorded case can only ever reach `plan::render`'s plain-string
    /// fallback. The highlighted DAG in A-144's mock 5 snapshots was a picture of a payload the
    /// durable log does not contain.
    #[test]
    fn a_recorded_plan_has_no_ast_because_the_log_never_persisted_one() {
        for case in LOAD_CASES.iter().filter(|c| c.is_recorded()) {
            let plan = fixture(*case).plan;
            assert!(
                plan.get("plan_ast").is_none(),
                "{}: a recorded plan cannot have an AST",
                case.name(),
            );
            let src = plan["plan"].as_str().unwrap_or_default();
            assert!(
                src.starts_with("flow "),
                "{}: no durable plan_source to draw: {src:?}",
                case.name(),
            );
            assert!(
                !crate::plan::render(&plan, &crate::theme::Theme::MONO).is_empty(),
                "{}: the live plan renderer draws nothing for it",
                case.name(),
            );
        }
    }

    #[test]
    fn every_case_carries_the_load_it_claims() {
        // The two recorded cases, at the numbers the capture actually produces. Pinned rather than
        // bounded: a change here means the projection moved, and that is worth reading a diff over.
        let tidy = fixture(LoadCase::Tidy);
        assert_eq!(tidy.step_count(), 18, "one recorded turn");
        let long = fixture(LoadCase::LongRun);
        assert_eq!(long.step_count(), 191, "nine recorded turns");
        assert_eq!(long.steps.len(), 9, "one rail row per turn");

        let deep = fixture(LoadCase::DeepNesting);
        let depth = deep.flatten().iter().map(|f| f.depth).max().unwrap();
        assert!(depth >= 7, "deep nesting is only {depth} levels");
        let fan = fixture(LoadCase::FanOut);
        let concurrent = fan
            .flatten()
            .iter()
            .filter(|f| f.step.kind == StepKind::Spawn && f.step.status == Status::Running)
            .count();
        assert!(concurrent >= 5, "fan-out is only {concurrent} wide");
    }

    /// ⚠ **A-145's headline measurement.** A-144's recommendation was revised on review to
    /// "condense finished phases FIRST", on the reasoning that a finished phase costs one row
    /// however many steps ran inside it. That reasoning is only worth what the *phase* distribution
    /// is worth — and the hand-authored fixture's phases were tidy because somebody wrote tidy
    /// phases (3 to 6 children each, evenly).
    ///
    /// A real session's are not. This pins the distribution so the claim is anchored to a
    /// measurement rather than to a fixture's manners.
    #[test]
    fn a_real_runs_phases_are_lumpy_and_the_hand_authored_ones_were_not() {
        let long = fixture(LoadCase::LongRun);
        let mut phases: Vec<usize> = long
            .steps
            .iter()
            .flat_map(|turn| turn.children.iter().map(Step::count))
            .collect();
        phases.sort_unstable();
        let singletons = phases.iter().filter(|&&n| n == 1).count();
        let biggest = *phases.last().expect("phases");

        // Two thirds of the real phases are a single step — condensing them saves nothing…
        assert!(
            singletons * 3 >= phases.len() * 2,
            "{singletons} of {} real phases are one step",
            phases.len(),
        );
        // …while one is fifty-seven, which is where the entire win lives.
        assert!(
            biggest >= 50,
            "the biggest real phase is only {biggest} steps",
        );
        // The hand-authored cases have neither shape: no singleton-heavy tail, no outlier.
        let authored: Vec<usize> = fixture(LoadCase::FanOut)
            .steps
            .iter()
            .flat_map(|r| r.children.iter().map(Step::count))
            .collect();
        assert!(
            authored.iter().max().copied().unwrap_or(0) < 10,
            "the hand-authored fixture grew an outlier phase: {authored:?}",
        );
    }

    /// The provenance badge is on screen, not only in a doc comment — the C-422 discipline applied
    /// to this artifact itself.
    #[test]
    fn every_render_says_whether_its_load_was_measured_or_invented() {
        for case in LOAD_CASES {
            let fx = fixture(case);
            assert_eq!(
                matches!(fx.provenance, Provenance::Recorded { .. }),
                case.is_recorded(),
                "{}: provenance disagrees with the case",
                case.name(),
            );
            let badge = fx.provenance.badge();
            for mock in super::super::MOCKS {
                let plain = super::super::render(
                    mock,
                    case,
                    super::super::WIDE,
                    &crate::theme::Theme::MONO,
                )
                .to_plain();
                assert!(
                    plain.contains(&badge),
                    "{} / {}: no provenance badge {badge:?} on screen\n{plain}",
                    mock.spec().name,
                    case.name(),
                );
            }
        }
    }

    #[test]
    fn ids_are_a_stable_pre_order() {
        let fx = fixture(LoadCase::Tidy);
        let ids: Vec<usize> = fx.flatten().iter().map(|f| f.step.id).collect();
        assert_eq!(ids, (0..fx.step_count()).collect::<Vec<_>>());
    }

    #[test]
    fn focus_lands_on_the_deepest_running_step() {
        let fx = fixture(LoadCase::DeepNesting);
        assert_eq!(fx.focused().label, "model.decide");
        // audit > tracker-audit#1 > scan > story-check#1 > read-epic > model.decide
        assert_eq!(fx.path_to(fx.focused().id).len(), 6);
    }
}
