//! Mock 5 — **graph-first.** The authored Flux-Lang program is the primary object; the run
//! annotates it.
//!
//! The bet is the epic's own claim: flux's point is that the model is called *from* a program, and
//! the fastest way to make that visible is to show the program and let the run happen inside it.
//! Nobody has to be told the LLM is not the runtime when `model.decide` is one highlighted node in
//! a DAG whose other eight nodes are executing without it.
//!
//! **This mock reuses the renderer A-138 will actually use.** The plan tree comes from
//! [`crate::plan::render`] — the same function the live plan card calls, preferring `plan_ast` and
//! syntax-highlighting through `flux_flow::render::render_styled` — and this module only prepends a
//! status gutter. Nothing about the tree is re-drawn here, which is the whole reason to trust what
//! it looks like.
//!
//! The gutter is a line-for-line table over that renderer's output ([`GUTTER`]).
//! `the_graph_mock_annotates_the_renderer_a_138_will_actually_use` fails if the two ever fall out
//! of step, rather than letting the annotations slide quietly off their nodes.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::{
    clip, header, ms, status_style, width, Axis, Fixture, MockSpec, Status, Step, Tally, Viewport,
    PAUSE_GLYPH,
};
use crate::theme::Theme;

pub(super) const SPEC: MockSpec = MockSpec {
    name: "5 · graph-first",
    axis: Axis::Program,
    optimizes_for: "making the architecture visible — the model call is one typed node in an \
                    authored DAG, and the run is annotation on top of it. It is also the only \
                    layout whose height is fixed by the program rather than by the run",
    gives_up: "the present tense. A program does not move, so most rows are always the parts that \
               are not running; a loop node collapses N runtime steps into one row, and a \
               sub-agent's own program is not in this program at all. Dynamic structure — six \
               workers, a retry, a nested spawn — is exactly what it cannot draw",
    pause_affordance: "the status gutter, on the node currently executing — a pause here reads as \
                       a breakpoint on a line of the program, which is what A-140 would want",
    inspection_pane:
        "a bottom split showing the node's runtime steps — which is also the only way \
                      this layout can show them at all",
    min_cols: 48,
    // The program has a fixed height, so too few rows shows a fragment of a shape rather than the
    // shape — which would flatter the one layout whose selling point is that it always fits.
    min_rows: 8,
};

/// Which fixture step annotates each line of [`crate::plan::render`]'s output, by label prefix.
/// `""` is a structural line (a header, an argument literal) that no runtime step corresponds to.
///
/// A prefix rather than an exact label because one program node routinely stands for several
/// runtime steps — the `task` inside the `each` is six workers in the fan-out case — and
/// collapsing them into one annotated row is the behaviour this mock exists to show.
pub(super) const GUTTER: &[&str] = &[
    "plan",              // the `plan · risk · N ops` header plan.rs draws first
    "",                  // flow tracking_sync
    "validate",          // ├─ seq -> $validated
    "glob",              // │  ├─ $stories = glob(...)
    "read",              // │  ├─ $template = read(...)
    "track.frontmatter", // │  └─ $meta = track.frontmatter({files: $stories})
    "model.decide",      // ├─ $verdict = model.decide(...)   ← the bounded semantic slot
    "audit",             // ├─ each $epic in $verdict -> $audits
    "tracker-audit",     // │  └─ task(role: tracker-audit)   ← one node, N workers
    "write",             // ├─ write(docs/stories/README.md)
    "edit",              // └─ edit(CHANGELOG.md)
];

/// Width of the `✓  120ms ` status gutter cell, before its `│ ` separator.
const GUTTER_COLS: usize = 10;

/// The gutter table for one fixture: which step label prefix annotates each line of
/// [`crate::plan::render`]'s output.
///
/// ⚠ **A-145.** A hand-authored fixture can be given a nine-node program and a hand-written table
/// to match it. A recorded one cannot: this loop emits **one accepted plan per op**, so a real
/// "program" is a single call and its gutter is two rows — the `plan` header and that one line.
/// Deriving the table instead of declaring it is what makes the recorded case drawable at all, and
/// what makes the resulting picture the honest one.
fn gutter(fx: &Fixture) -> Vec<String> {
    if !matches!(fx.provenance, super::Provenance::Recorded { .. }) {
        return GUTTER.iter().map(|s| s.to_string()).collect();
    }
    // `plan · … · N op`, then the flattened `plan_source`. The op the recorded plan calls is the
    // only thing on that line worth annotating, and it is named after the `=`.
    let src = fx.plan.get("plan").and_then(|v| v.as_str()).unwrap_or("");
    let op = src
        .split_once('=')
        .map(|(_, rhs)| rhs.trim())
        .and_then(|rhs| rhs.split('(').next())
        .map(str::trim)
        .unwrap_or("");
    // The renderer emits header + 1 for a one-line plan string.
    vec![String::new(), op.to_string()]
}

/// How many lines the plan renderer emits for a fixture's plan observation. Exposed so the test can
/// compare it with the gutter's length, per case.
pub fn graph_plan_line_count(case: super::LoadCase) -> usize {
    crate::plan::render(&super::fixture(case).plan, &Theme::MONO).len()
}

/// The gutter's length for a case. See [`graph_plan_line_count`].
pub fn graph_gutter_len(case: super::LoadCase) -> usize {
    gutter(&super::fixture(case)).len()
}

pub(super) fn draw(
    fx: &Fixture,
    vp: Viewport,
    theme: &Theme,
    tally: &mut Tally,
) -> Vec<Line<'static>> {
    let mut out = vec![header(fx, vp, theme, "graph-first")];
    let budget = vp.rows.saturating_sub(out.len() + tally.footer_rows());

    let plan = crate::plan::render(&fx.plan, theme);
    let table = gutter(fx);
    let flat = fx.flatten();
    let mut drawn: Vec<usize> = Vec::new();

    for (i, line) in plan.into_iter().take(budget).enumerate() {
        let prefix = table.get(i).map(String::as_str).unwrap_or("");
        let matches: Vec<&Step> = if prefix.is_empty() {
            Vec::new()
        } else {
            flat.iter()
                .map(|f| f.step)
                .filter(|s| s.label.starts_with(prefix))
                .collect()
        };
        let gutter = annotate(&matches, theme);
        for step in &matches {
            drawn.push(step.id);
        }
        let mut spans = gutter;
        spans.extend(line.spans);
        out.push(clip(Line::from(spans), vp.cols));
    }

    tally.drew(drawn);

    if out.len() < vp.rows.saturating_sub(tally.footer_rows()) {
        out.push(clip(
            Line::from(Span::styled(
                format!("{PAUSE_GLYPH} breaks on the running node · ↵ opens its runtime steps"),
                theme.muted_style(),
            )),
            vp.cols,
        ));
    }
    out
}

/// The status gutter for one program node: the aggregate of every runtime step it stands for.
/// `3×` in front of a count is the honest form of a loop node — one row, several executions.
fn annotate(matches: &[&Step], theme: &Theme) -> Vec<Span<'static>> {
    // The separator is drawn even for an unannotated line: a gutter that disappears where nothing
    // ran would make the program's own indentation jump.
    let sep = Span::styled("│ ", Style::default());
    if matches.is_empty() {
        return vec![Span::raw(" ".repeat(GUTTER_COLS)), sep];
    }
    let running = matches.iter().any(|s| s.status == Status::Running);
    let failed = matches.iter().any(|s| s.status == Status::Failed);
    let pending = matches.iter().all(|s| s.status == Status::Pending);
    let status = if failed {
        Status::Failed
    } else if running {
        Status::Running
    } else if pending {
        Status::Pending
    } else {
        Status::Done
    };
    let total: u64 = matches.iter().map(|s| s.dur_ms).sum();
    let time = if status == Status::Pending {
        "—".to_string()
    } else if matches.len() > 1 {
        format!("{}×{}", matches.len(), ms(total / matches.len() as u64))
    } else {
        ms(total)
    };
    let mark = if running { PAUSE_GLYPH } else { status.glyph() };
    let cell = format!("{mark} {time:>7} ");
    let cell = if width(&cell) > GUTTER_COLS {
        format!("{mark} {:>7} ", "…")
    } else {
        cell
    };
    vec![
        Span::styled(
            format!("{cell:<GUTTER_COLS$}"),
            status_style(status, theme).add_modifier(if running {
                Modifier::BOLD
            } else {
                Modifier::empty()
            }),
        ),
        sep,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loopmock::{self, LoadCase, Mock};

    fn program_lines(case: LoadCase) -> Vec<String> {
        loopmock::render(Mock::Graph, case, loopmock::WIDE, &Theme::MONO)
            .to_plain()
            .lines()
            .filter_map(|l| l.split_once('│').map(|(_, right)| right.to_string()))
            .collect()
    }

    #[test]
    fn the_program_is_the_same_drawing_under_every_hand_authored_case() {
        // A-144's finding, still true where it was measured: for an authored DAG the program does
        // not move — only the gutter does. That is both the strength (fixed height) and the
        // weakness (a fan-out of six is still one row).
        assert_eq!(
            program_lines(LoadCase::DeepNesting),
            program_lines(LoadCase::FanOut)
        );
    }

    /// ⚠ **A-145's second correction to A-144.** "The program does not move" was the whole case for
    /// mock 5 — a fixed-height layout you can read like source. It is a property of the
    /// hand-authored fixture, which had one nine-node program for the whole run.
    ///
    /// A real adaptive run re-authors its program **per op**: the captured session accepted 127
    /// plans and every one of them is a single call. So the program moves constantly, and it is
    /// never bigger than one line. The layout whose premise was "show the authored DAG" has, on a
    /// real run, no DAG to show.
    #[test]
    fn a_real_runs_program_is_one_op_and_it_moves_every_step() {
        let tidy = program_lines(LoadCase::Tidy);
        let long = program_lines(LoadCase::LongRun);
        assert_ne!(
            tidy, long,
            "the recorded program held still — the capture would have to be single-plan for that",
        );
        for (case, lines) in [(LoadCase::Tidy, &tidy), (LoadCase::LongRun, &long)] {
            let plan_rows = lines.iter().filter(|l| l.contains("flow ")).count();
            assert_eq!(
                plan_rows,
                1,
                "{}: a recorded plan is one op, so it is one row: {lines:?}",
                case.name(),
            );
        }
    }

    #[test]
    fn a_loop_node_says_how_many_executions_it_stands_for() {
        let plain = loopmock::render(Mock::Graph, LoadCase::FanOut, loopmock::WIDE, &Theme::MONO)
            .to_plain();
        assert!(
            plain.contains("6×"),
            "the six workers collapsed silently:\n{plain}"
        );
    }

    #[test]
    fn the_model_call_is_visibly_one_node_among_the_rest() {
        let plain = loopmock::render(Mock::Graph, LoadCase::FanOut, loopmock::WIDE, &Theme::MONO)
            .to_plain();
        assert!(plain.contains("model.decide"), "{plain}");
        assert!(plain.contains("track.frontmatter"), "{plain}");
    }
}
