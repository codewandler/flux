//! Mock 3 — **thread plus detail pane.** A condensed rail on the left, everything about the step
//! in focus on the right.
//!
//! The bet: a live run has exactly one interesting step at a time and the rest is context, so the
//! rail condenses finished phases to one row each and expands only the one that is running — which
//! is the design doc's own description of A-137 — while the depth, the arguments, the token cost
//! and the live output go in the pane, the only place in these five layouts with room for them.
//!
//! It is the only layout here that needs a *selection*, which is both its advantage (A-142's
//! inspection pane already exists as the right column) and its cost (A-137 has to decide a focus
//! policy before it renders correctly at all).

use std::collections::BTreeSet;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::{
    clip, header, ms, pad, shorten, status_style, tokens, width, window, Axis, Fixture, Flat,
    MockSpec, Status, Step, Tally, Viewport, DETAIL, PAUSE_GLYPH,
};
use crate::theme::Theme;

pub(super) const SPEC: MockSpec = MockSpec {
    name: "3 · thread + detail pane",
    axis: Axis::Selection,
    optimizes_for: "keeping one step fully legible while the run moves — finished phases cost one \
                    row each however many steps ran inside them, and the pane is the only place in \
                    these five with room for arguments, token cost and live output at once",
    gives_up: "width (64 cols, the highest floor of the five) and one narrative — a rail beside a \
               pane is not something you scroll back through. It also needs a focus policy before \
               it renders at all, which none of the other four do",
    pause_affordance: "the focused rail row, where the ⏸ sits beside the step it would pause",
    inspection_pane: "already built: the right-hand pane *is* A-142's inspection surface",
    min_cols: MIN_COLS,
    min_rows: MIN_ROWS,
};

/// This layout's width floor — the highest of the five, and A-144's single biggest charge against
/// it. Named since A-146 because [`super::axes`] needed to ask whether the floor belongs to the
/// *layout* or to the *pane*; it borrows the pane, so it borrows the floor with it.
pub(super) const MIN_COLS: usize = 64;
/// The pane spends [`PANE_CHROME`] rows before it says anything, and must still hold a body row, an
/// elision marker and the hint; plus the run header and the step footer.
pub(super) const MIN_ROWS: usize = 10;

/// Rows the pane spends on chrome before any content: breadcrumb, header, timing/usage, rule.
/// The one number behind this mock's row floor — and the reason it has the highest one.
const PANE_CHROME: usize = 4;

/// The rail is a share of the terminal, clamped so it neither starves the pane nor becomes a
/// column of ellipses.
pub(super) fn rail_cols(cols: usize) -> usize {
    (cols * 2 / 5).clamp(26, 42)
}

/// Deepest indent the rail spends columns on; past it the rail stops indenting and lets the pane
/// carry the structure.
const RAIL_MAX_INDENT: usize = 3;

pub(super) fn draw(
    fx: &Fixture,
    vp: Viewport,
    theme: &Theme,
    tally: &mut Tally,
) -> Vec<Line<'static>> {
    let mut out = vec![header(fx, vp, theme, "thread + detail pane")];
    let budget = vp.rows.saturating_sub(out.len() + tally.footer_rows());
    let rail = rail_cols(vp.cols);
    let pane = vp.cols.saturating_sub(rail + 3);

    let focused = fx.focused();
    let mut seen: BTreeSet<usize> = BTreeSet::new();

    let left = rail_rows(fx, focused, rail, budget, theme, &mut seen);
    let (right, hidden_detail) = pane_rows(fx, focused, pane, budget, theme, &mut seen);

    // Only as tall as the taller column: a split that padded itself out to the terminal would be
    // showing empty rows as if they meant something.
    let rows = budget.min(left.len().max(right.len()));
    for row in 0..rows {
        let l = left.get(row).cloned().unwrap_or_default();
        let r = right.get(row).cloned().unwrap_or_default();
        let mut spans = pad(l, rail).spans;
        spans.push(Span::styled(" │ ", theme.muted_style()));
        spans.extend(r.spans);
        out.push(clip(Line::from(spans), vp.cols));
    }

    tally.drew(seen.iter().copied());
    if hidden_detail > 0 {
        tally.hid(DETAIL, hidden_detail, format!("+{hidden_detail} more"));
    }
    out
}

/// The left rail: every top-level step condensed to one row, plus the descendants of the one the
/// focus is inside. This is progressive disclosure applied to the thread itself — finished phases
/// cost one row each whatever ran in them, so the rail's height tracks the *program* rather than
/// the run.
fn rail_rows(
    fx: &Fixture,
    focused: &Step,
    cols: usize,
    budget: usize,
    theme: &Theme,
    seen: &mut BTreeSet<usize>,
) -> Vec<Line<'static>> {
    let root = fx
        .steps
        .iter()
        .position(|s| contains(s, focused.id))
        .unwrap_or(0);
    let rows: Vec<Flat<'_>> = fx
        .flatten()
        .into_iter()
        .filter(|f| f.depth == 0 || contains(&fx.steps[root], f.step.id))
        .collect();
    let focus_idx = rows
        .iter()
        .position(|f| f.step.id == focused.id)
        .unwrap_or(0);
    let (start, rows) = window(&rows, budget, focus_idx);

    let mut out = Vec::new();
    if start > 0 {
        out.push(clip(
            Line::from(Span::styled(
                format!("↑ {start} above"),
                theme.muted_style(),
            )),
            cols,
        ));
    }
    for flat in rows.iter().skip(usize::from(start > 0)) {
        let step = flat.step;
        seen.insert(step.id);
        let on_path = contains(step, focused.id);
        let pause = if step.id == focused.id {
            PAUSE_GLYPH
        } else {
            " "
        };
        let elapsed = match step.status {
            Status::Pending => "pending".to_string(),
            _ => ms(step.dur_ms),
        };
        let label = format!(
            "{}{} {} {}",
            "  ".repeat(flat.depth.min(RAIL_MAX_INDENT)),
            step.status.glyph(),
            step.kind.sigil(),
            step.label
        );
        let style = if on_path {
            status_style(step.status, theme).add_modifier(Modifier::BOLD)
        } else {
            status_style(step.status, theme)
        };
        let room = cols.saturating_sub(2 + width(&elapsed));
        let label = shorten(&label, room);
        let gap = cols.saturating_sub(width(&label) + 1 + width(&elapsed));
        out.push(clip(
            Line::from(vec![
                Span::styled(label, style),
                Span::raw(" ".repeat(gap)),
                Span::styled(pause.to_string(), theme.warn_style()),
                Span::styled(elapsed, theme.muted_style()),
            ]),
            cols,
        ));
    }
    out
}

/// The right pane: the focused step in full — breadcrumb, timing, token cost, the live output tail
/// (`UiEvent::ToolProgress`), and its own children with the tree mock's connectors.
///
/// The breadcrumb is the pane's answer to the tree's indentation: the ancestors are named once, at
/// the top, instead of costing three columns on every row beneath them.
pub(super) fn pane_rows(
    fx: &Fixture,
    focused: &Step,
    cols: usize,
    budget: usize,
    theme: &Theme,
    seen: &mut BTreeSet<usize>,
) -> (Vec<Line<'static>>, usize) {
    let mut out: Vec<Line<'static>> = Vec::new();
    for step in ancestors(fx, focused.id) {
        seen.insert(step);
    }
    seen.insert(focused.id);

    out.push(clip(
        Line::from(Span::styled(
            fx.path_to(focused.id).join(" › "),
            theme.muted_style(),
        )),
        cols,
    ));

    let mut head = vec![
        Span::styled(
            format!("{} {} ", focused.status.glyph(), focused.kind.sigil()),
            status_style(focused.status, theme),
        ),
        Span::styled(
            focused.label.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  {}", focused.detail), Style::default()),
    ];
    let elapsed = match focused.status {
        Status::Pending => "pending".to_string(),
        _ => ms(focused.dur_ms),
    };
    let used: usize = head.iter().map(|s| width(&s.content)).sum();
    head.push(Span::raw(
        " ".repeat(cols.saturating_sub(used + width(&elapsed))),
    ));
    head.push(Span::styled(elapsed, theme.muted_style()));
    out.push(clip(Line::from(head), cols));

    let mut meta = format!("started +{}", ms(focused.start_ms));
    if let Some(u) = focused.usage {
        meta.push_str(&format!(
            "   in {} · out {} · cache {}",
            tokens(u.input),
            tokens(u.output),
            tokens(u.cached)
        ));
    }
    out.push(clip(
        Line::from(Span::styled(meta, theme.muted_style())),
        cols,
    ));
    out.push(clip(
        Line::from(Span::styled("─".repeat(cols), theme.muted_style())),
        cols,
    ));

    // Everything below competes for the same rows: the output tail first, then the children.
    let mut body: Vec<(String, Option<usize>)> = focused
        .note
        .lines()
        .map(|l| (format!("  {l}"), None))
        .collect();
    for (i, child) in focused.children.iter().enumerate() {
        let last = i + 1 == focused.children.len();
        body.push((
            format!(
                "{} {} {} {} · {}",
                if last { "└─" } else { "├─" },
                child.status.glyph(),
                child.kind.sigil(),
                child.label,
                child.detail
            ),
            Some(child.id),
        ));
    }

    // Rows left for the body once the chrome and the hint have been paid for. `draw` composes at
    // most `budget` rows, so a pane taller than that would push its own `+N more` marker off the
    // screen while still reporting the elision — an undisclosed truncation, which is the one thing
    // this module exists not to do. `min_rows` is what keeps this at least 3.
    let room = budget.saturating_sub(PANE_CHROME + 1);
    let hidden = body.len().saturating_sub(room);
    let shown = if hidden > 0 {
        room.saturating_sub(1)
    } else {
        room
    };
    for (text, id) in body.iter().take(shown) {
        if let Some(id) = id {
            seen.insert(*id);
        }
        out.push(clip(
            Line::from(Span::styled(shorten(text, cols), theme.panel_style())),
            cols,
        ));
    }
    if hidden > 0 {
        out.push(clip(
            Line::from(Span::styled(format!("+{hidden} more"), theme.warn_style())),
            cols,
        ));
    }
    out.push(clip(
        Line::from(Span::styled(
            format!("{PAUSE_GLYPH} pause here · ↹ move focus · ↵ expand"),
            theme.muted_style(),
        )),
        cols,
    ));

    // Chrome + shown + marker + hint is <= budget by construction of `room`, for every viewport
    // `render` accepts. This trims the chrome rather than the marker if that ever stops holding —
    // the timing line and the rule are the pane's least load-bearing rows, and losing one of them
    // is a worse drawing, while losing the marker would be a lie.
    while out.len() > budget && out.len() > 2 {
        out.remove(PANE_CHROME - 1);
    }
    (out, hidden)
}

fn contains(step: &Step, id: usize) -> bool {
    step.id == id || step.children.iter().any(|c| contains(c, id))
}

/// Ids of `id`'s ancestors — drawn as the pane's breadcrumb, so they count as represented.
fn ancestors(fx: &Fixture, id: usize) -> Vec<usize> {
    fn walk(steps: &[Step], id: usize, trail: &mut Vec<usize>) -> bool {
        for step in steps {
            trail.push(step.id);
            if step.id == id || walk(&step.children, id, trail) {
                return true;
            }
            trail.pop();
        }
        false
    }
    let mut trail = Vec::new();
    walk(&fx.steps, id, &mut trail);
    trail
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loopmock::{self, LoadCase, Mock};

    /// ⚠ **A-145's third correction, and the one that matters most.** The recommendation rests on
    /// "a finished phase costs one row however many steps ran inside it", so the rail's height
    /// tracks the program rather than the run. Against the hand-authored fixture that held: one
    /// run, nine top-level rows, whatever the step count.
    ///
    /// Against a real *session* it does not, and the reason is structural rather than a tuning
    /// miss: the depth-0 unit of a recorded session is a **turn**, and turns accumulate. Nine turns
    /// is nine rail rows before a single step is drawn, and the focused turn then expands in full.
    ///
    /// The condensing still does the bulk of the work — this pins how much — but the claim has to
    /// be stated as "sub-linear in steps", not "constant".
    /// The uncondensed rail for a case — the condensing rule on its own, with the terminal's
    /// windowing taken out of the way.
    fn rail_height(case: LoadCase) -> usize {
        let fx = loopmock::fixture(case);
        rail_rows(
            &fx,
            fx.focused(),
            rail_cols(loopmock::WIDE.cols),
            usize::MAX,
            &Theme::MONO,
            &mut BTreeSet::new(),
        )
        .len()
    }

    /// ⚠ **A-145's re-check of the claim the whole recommendation now rests on.** After review,
    /// A-144's headline became "condense finished phases FIRST" — because a finished phase costs
    /// one row however many steps ran inside it. Measured against a real session, that is
    /// *substantially* true and *not* the constant the fixture implied:
    ///
    /// - 191 recorded steps condense to a rail of ~25 rows, a 7× saving, which is the win;
    /// - but the depth-0 unit of a real session is a **turn**, and turns accumulate — nine of them
    ///   here — and the focused turn then expands in full. So the rail is sub-linear in steps, not
    ///   constant, and at 20 rows it already has to scroll.
    #[test]
    fn condensing_makes_a_real_session_sub_linear_but_not_constant() {
        let long = loopmock::fixture(LoadCase::LongRun);
        let rail = rail_height(LoadCase::LongRun);

        // The win, measured: the rail is a small fraction of the flat thread's height.
        assert!(
            rail * 4 < long.step_count(),
            "the rail costs {rail} rows for {} steps — barely condensed at all",
            long.step_count(),
        );
        // Not constant: it is one row per turn plus the focused turn in full.
        assert!(
            rail > long.steps.len(),
            "the rail ({rail}) never expanded the focused turn beyond the {} turn rows",
            long.steps.len(),
        );
        assert!(
            rail > rail_height(LoadCase::Tidy),
            "nine turns cost the same rail as one; then the fixture, not the layout, was condensing",
        );
        // And it still outgrows the terminal this is most often read in.
        assert!(
            rail > loopmock::NARROW.rows,
            "a {rail}-row rail fits a {}-row terminal — the scrolling claim would be wrong",
            loopmock::NARROW.rows,
        );
    }

    #[test]
    fn the_pane_shows_what_no_single_row_could_carry() {
        let plain = loopmock::render(
            Mock::Split,
            LoadCase::DeepNesting,
            loopmock::WIDE,
            &Theme::MONO,
        )
        .to_plain();
        assert!(
            plain.contains("cache"),
            "no token cost in the pane:\n{plain}"
        );
        assert!(plain.contains(" › "), "no breadcrumb in the pane:\n{plain}");
    }

    #[test]
    fn the_rail_expands_only_the_phase_the_focus_is_in() {
        let fx = loopmock::fixture(LoadCase::LongRun);
        let rail: String = rail_rows(
            &fx,
            fx.focused(),
            rail_cols(loopmock::WIDE.cols),
            usize::MAX,
            &Theme::MONO,
            &mut BTreeSet::new(),
        )
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
        .collect();

        // Every turn is on the rail, condensed to one row…
        for turn in 1..=9 {
            assert!(rail.contains(&format!("turn {turn}")), "{rail}");
        }
        // …the focused one expanded into its own phases…
        assert!(rail.contains("present_results"), "{rail}");
        // …and nothing from an unfocused turn reaches it — turn 7 is where the commit happened.
        assert!(!rail.contains("git_commit"), "{rail}");
    }

    #[test]
    fn a_truncated_detail_body_says_how_much_it_kept_back() {
        let r = loopmock::render(
            Mock::Split,
            LoadCase::FanOut,
            loopmock::Viewport { cols: 70, rows: 9 },
            &Theme::MONO,
        );
        for elision in r.elisions.iter().filter(|e| e.what == loopmock::DETAIL) {
            assert!(r.to_plain().contains(&elision.marker));
        }
    }
}
