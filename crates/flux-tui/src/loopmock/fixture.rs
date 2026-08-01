//! The one hand-authored flow all five mocks draw, plus the three load cases that decide the
//! comparison.
//!
//! The flow is the tracking-plugin one — validate story frontmatter, regenerate the board, audit
//! epics for missing trackers, sync the CHANGELOG — with a model-authored judgement step in the
//! middle. It is a flow the team already reads every week, and it is the same one C-425 proposes
//! as the flagship recipe, so the shapes here are the shapes A-137 will actually have to draw.
//!
//! ⚠ **This is hand-written data, not a captured run.** The timings are plausible, not measured.
//! What the fixture is good for is layout pressure — step counts, nesting depth, concurrent
//! children, label lengths — and that is all it is used for.
//!
//! Every step maps onto a shape `UiEvent` already carries
//! (`crates/flux-tui/src/controller.rs`): [`StepKind::Plan`] is `Plan`, [`StepKind::Phase`] is
//! `Phase`, [`StepKind::Tool`] is `ToolCall`/`ToolTiming`/`ToolResult`, [`StepKind::Model`] is
//! `Intent` + `CallUsage`, [`StepKind::Spawn`] is `SpawnActivity`. Nothing here invents a new
//! event.

use serde_json::{json, Value};

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
    /// The flow as it reads in a demo: 15 steps, mid-run, everything visible.
    Tidy,
    /// The flow as it reads on a real board: 49 steps across nine phases — more than fits.
    LongRun,
    /// Nested delegation: a sub-agent that spawns a sub-agent, seven levels down.
    DeepNesting,
    /// Six tracker-audit workers running at once, each mid-op.
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
    /// the same payload shape the live TUI already receives, `plan_ast` included.
    pub plan: Value,
}

impl Fixture {
    fn new(title: &str, elapsed_ms: u64, mut steps: Vec<Step>) -> Self {
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
            plan: plan_observation(),
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
        LoadCase::Tidy => tidy(),
        LoadCase::LongRun => long_run(),
        LoadCase::DeepNesting => deep_nesting(),
        LoadCase::FanOut => fan_out(),
    }
}

/// 15 steps, mid-run: three phases finished, the audit fan-out running, two phases pending. What a
/// demo shows, and — on its own — what proves nothing.
fn tidy() -> Fixture {
    use Status::*;
    use StepKind::*;
    Fixture::new(
        "tracking board sync",
        12_400,
        vec![
            Step::new(Plan, "plan", "low · mutating · 9 ops", Done, 0, 140)
                .note("frozen from .flux/flows/tracking-sync.flux — 9 ops, 1 model call, 1 fan-out"),
            Step::new(Phase, "validate", "14 stories · 2 missing `epic:`", Done, 140, 500).kids(vec![
                Step::new(Tool, "glob", "docs/stories/*.md → 143 files", Done, 145, 120),
                Step::new(Tool, "read", "docs/stories/_TEMPLATE.md → 41 lines", Done, 265, 12),
                Step::new(Tool, "track.frontmatter", "143 parsed · 2 missing `epic:`", Done, 277, 363)
                    .note("A-131 missing `epic:`\nC-388 missing `epic:`\nremaining 141 well-formed"),
            ]),
            Step::new(Phase, "judge", "3 epics untracked, 2 ambiguous", Done, 640, 4_260).kids(vec![
                Step::new(Model, "model.decide", "opus · judge · round 3 · 2 ops", Done, 660, 4_220)
                    .note(
                        "the bounded semantic slot: which epics lack a tracker story, and which of\n\
                         those are ambiguous enough to leave alone. Returns two lists; the runtime\n\
                         decides what happens to them.",
                    )
                    .usage(8_420, 512, 6_100),
            ]),
            Step::new(Phase, "audit", "6 workers · 4 running", Running, 4_900, 7_500).kids(vec![
                Step::new(Spawn, "tracker-audit#1", "agent-loop-visibility → 1 gap", Done, 4_950, 3_150)
                    .note("A-137 tracked · A-138 tracked · A-139 tracked\nA-144 tracked\ngap: no story covers the fleet pane's fold"),
                Step::new(Spawn, "tracker-audit#2", "flux-lang-hardening · grep", Running, 4_960, 7_440)
                    .note("grep \"epic: flux-lang-hardening\" docs/stories\nL-002 · L-003 · L-004 · L-007\nstill reading L-011…"),
                Step::new(Spawn, "tracker-audit#3", "syntax-simplification · read", Running, 4_970, 7_430)
                    .note("docs/designs/syntax-simplification.md\n  4 stories claim this epic\n  1 design section has no story\nreading docs/stories/L-014…"),
            ]),
            Step::new(Phase, "board", "regenerate the generated region", Pending, 0, 0)
                .kids(vec![Step::new(Tool, "write", "docs/stories/README.md", Pending, 0, 0)]),
            Step::new(Phase, "changelog", "sync [Unreleased]", Pending, 0, 0)
                .kids(vec![Step::new(Tool, "edit", "CHANGELOG.md · 2 edits", Pending, 0, 0)]),
        ],
    )
}

/// 49 steps across nine phases — dozens of steps, comfortably more than any viewport holds. This
/// is the case that separates the layouts whose cost is per-step from the ones whose cost is
/// per-phase.
fn long_run() -> Fixture {
    use Status::*;
    use StepKind::*;
    let files = [
        "A-131-the-fleet-pane.md",
        "A-137-the-step-thread.md",
        "A-138-expand-a-step-into-its-graph.md",
        "A-139-the-loop-view-under-load.md",
        "C-388-plugin-pack-signing.md",
        "C-392-server-router-tests.md",
        "C-395-file-surface-port.md",
        "C-400-remove-the-downstream-name.md",
        "C-425-the-flagship-recipe.md",
    ];
    /// Build a phase whose children run back to back, advancing the run clock as it goes.
    fn phase(
        t: &mut u64,
        kind: StepKind,
        label: &str,
        detail: &str,
        status: Status,
        kids: Vec<(String, String, Status, u64)>,
    ) -> Step {
        let start = *t;
        let mut children = Vec::new();
        for (label, detail, status, dur) in kids {
            children.push(Step::new(StepKind::Tool, &label, &detail, status, *t, dur));
            *t += dur;
        }
        let total = *t - start;
        Step::new(kind, label, detail, status, start, total).kids(children)
    }

    let mut steps = vec![Step::new(
        Plan,
        "plan",
        "low · mutating · 31 ops",
        Done,
        0,
        180,
    )];
    let mut t = 180u64;
    steps.push(phase(
        &mut t,
        Phase,
        "scan",
        "143 story files",
        Done,
        files[..6]
            .iter()
            .map(|f| {
                (
                    "read".to_string(),
                    format!("docs/stories/{f}"),
                    Done,
                    9 + (f.len() as u64 % 7) * 11,
                )
            })
            .collect(),
    ));
    steps.push(phase(
        &mut t,
        Phase,
        "frontmatter",
        "8 checks · 2 failures",
        Done,
        (0..8)
            .map(|i| {
                (
                    "track.frontmatter".to_string(),
                    format!(
                        "{} → {}",
                        files[i % files.len()],
                        if i % 4 == 1 { "missing `epic:`" } else { "ok" }
                    ),
                    if i % 4 == 1 { Failed } else { Done },
                    34 + (i as u64) * 13,
                )
            })
            .collect(),
    ));
    let judge_start = t;
    steps.push(
        Step::new(
            Phase,
            "judge",
            "3 epics untracked",
            Done,
            judge_start,
            5_900,
        )
        .kids(vec![
            Step::new(
                Model,
                "model.decide",
                "opus · judge · round 3",
                Done,
                judge_start,
                4_220,
            )
            .usage(11_900, 640, 9_200),
            Step::new(
                Model,
                "model.decide",
                "opus · judge · round 4 (retry)",
                Done,
                judge_start + 4_220,
                1_680,
            )
            .usage(12_600, 210, 11_800),
        ]),
    );
    t = judge_start + 5_900;
    steps.push(phase(
        &mut t,
        Phase,
        "audit",
        "5 workers",
        Done,
        (1..=5)
            .map(|i| {
                (
                    format!("tracker-audit#{i}"),
                    format!("epic {i} → {} gap(s)", i % 3),
                    Done,
                    900 + (i as u64) * 240,
                )
            })
            .collect(),
    ));
    steps.push(phase(
        &mut t,
        Phase,
        "rewrite",
        "9 rows regenerated",
        Done,
        files
            .iter()
            .map(|f| ("edit".to_string(), format!("docs/stories/{f}"), Done, 18))
            .collect(),
    ));
    steps.push(phase(
        &mut t,
        Phase,
        "board",
        "generated region only",
        Done,
        vec![
            (
                "read".to_string(),
                "docs/stories/README.md".to_string(),
                Done,
                11,
            ),
            (
                "write".to_string(),
                "docs/stories/README.md · 18 234 bytes".to_string(),
                Done,
                27,
            ),
            (
                "bash".to_string(),
                "$ git diff --stat docs/stories/README.md".to_string(),
                Done,
                96,
            ),
        ],
    ));
    steps.push(phase(
        &mut t,
        Phase,
        "changelog",
        "sync [Unreleased]",
        Done,
        vec![
            ("read".to_string(), "CHANGELOG.md".to_string(), Done, 14),
            (
                "edit".to_string(),
                "CHANGELOG.md · 2 edits".to_string(),
                Done,
                22,
            ),
            (
                "grep".to_string(),
                "\"A-1\" in CHANGELOG.md".to_string(),
                Done,
                31,
            ),
        ],
    ));
    steps.push(phase(
        &mut t,
        Phase,
        "verify",
        "gate",
        Running,
        vec![
            (
                "bash".to_string(),
                "$ cargo fmt --all --check".to_string(),
                Done,
                640,
            ),
            (
                "bash".to_string(),
                "$ cargo test -p flux-codegate".to_string(),
                Done,
                8_100,
            ),
            (
                "bash".to_string(),
                "$ cargo clippy --workspace --all-targets".to_string(),
                Running,
                21_400,
            ),
            (
                "bash".to_string(),
                "$ bash scripts/build-portable-wasm.sh".to_string(),
                Pending,
                0,
            ),
        ],
    ));
    let elapsed = t;
    Fixture::new("tracking board sync · full regeneration", elapsed, steps)
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

/// Six tracker-audit workers at once, each mid-op — `SpawnActivity` at the width the fleet pane
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
    fn the_plan_observation_is_a_payload_the_live_plan_card_can_already_read() {
        // If this stops deserializing, the graph mock is drawing something the real renderer
        // would not — which would make it useless as evidence for A-138.
        let ast = fixture(LoadCase::Tidy).plan["plan_ast"].clone();
        let parsed: flux_flow::ast::DraftAst =
            serde_json::from_value(ast).expect("plan_ast is a DraftAst");
        assert_eq!(parsed.name.as_deref(), Some("tracking_sync"));
        assert_eq!(parsed.body.len(), 5);
    }

    #[test]
    fn every_case_carries_the_load_it_claims() {
        assert_eq!(fixture(LoadCase::Tidy).step_count(), 15);
        assert!(
            fixture(LoadCase::LongRun).step_count() >= 40,
            "the long run must not fit a viewport: {}",
            fixture(LoadCase::LongRun).step_count()
        );
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
