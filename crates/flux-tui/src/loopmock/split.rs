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
    min_cols: 64,
};

/// The rail is a share of the terminal, clamped so it neither starves the pane nor becomes a
/// column of ellipses.
fn rail_cols(cols: usize) -> usize {
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

    tally.drew(seen.len());
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
fn pane_rows(
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

    let room = budget.saturating_sub(out.len() + 1);
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

    #[test]
    fn the_rails_height_tracks_the_program_not_the_run_length() {
        // The claim the recommendation rests on: a run three times as long does not cost the rail
        // three times the rows, because a finished phase stays one row.
        let tidy = loopmock::fixture(LoadCase::Tidy);
        let long = loopmock::fixture(LoadCase::LongRun);
        assert!(long.step_count() > tidy.step_count() * 3);
        assert!(
            long.steps.len() <= tidy.steps.len() + 3,
            "the rail grew from {} to {} rows",
            tidy.steps.len(),
            long.steps.len()
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
        let plain =
            loopmock::render(Mock::Split, LoadCase::Tidy, loopmock::WIDE, &Theme::MONO).to_plain();
        // The running phase's workers are on the rail…
        assert!(plain.contains("tracker-audit#2"), "{plain}");
        // …while a finished phase's tool calls are not.
        assert!(!plain.contains("_TEMPLATE"), "{plain}");
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
