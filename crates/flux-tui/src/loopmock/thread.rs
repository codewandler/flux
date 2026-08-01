//! Mock 1 — **the flat thread.** One line per step, no indentation; structure lives in a
//! `phase/op` scope column instead of in whitespace.
//!
//! The bet: a loop is read the way a log is read — newest at the bottom, one row per thing that
//! happened — and every column spent on indentation is a column not spent on *what* happened. So
//! nesting is encoded, not drawn, and the row cost is exactly one line per step at every depth.
//!
//! This is the baseline A-137 proposes, and it is here so the other four have to beat something.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::{
    clip, fit, header, ms, status_style, width, window, Axis, Fixture, MockSpec, Status, Tally,
    Viewport, MIN_ROWS, PAUSE_GLYPH,
};
use crate::theme::Theme;

pub(super) const SPEC: MockSpec = MockSpec {
    name: "1 · flat thread",
    axis: Axis::Sequence,
    optimizes_for: "density and scanning — one row per step at any depth, so the whole run is the \
                    same shape whether it is flat or six levels deep",
    gives_up: "structure. The scope column is the first thing width pressure eats, and it gets \
               squeezed exactly when there is enough nesting for it to matter; concurrency reads \
               as interleaving rather than as parallelism",
    pause_affordance: "a ⏸ column between detail and elapsed, on the running row",
    inspection_pane: "the thread is one column, so inspection has to be a sheet over it (like the \
                      approval sheet) or a bottom split — it cannot sit beside the thread",
    min_cols: 40,
    // Header, the footer, and enough step rows that the thread reads as a thread. The lowest row
    // floor of the five, which is most of why it is the recommended fallback.
    min_rows: MIN_ROWS,
};

/// Widest the scope column is allowed to get before it is just stealing from the detail.
const MAX_SCOPE: usize = 34;
/// Right-hand elapsed column.
const TIME_COL: usize = 7;

pub(super) fn draw(
    fx: &Fixture,
    vp: Viewport,
    theme: &Theme,
    tally: &mut Tally,
) -> Vec<Line<'static>> {
    let mut out = vec![header(fx, vp, theme, "flat thread")];
    let budget = vp.rows.saturating_sub(out.len() + tally.footer_rows());

    let flat = fx.flatten();
    let focus = fx.focused().id;
    let focus_idx = flat.iter().position(|f| f.step.id == focus).unwrap_or(0);
    let (start, rows) = window(&flat, budget, focus_idx);
    if start > 0 {
        // A window is a form of elision even though the footer counts it; say so where the cut is.
        out.push(clip(
            Line::from(Span::styled(
                format!("  ↑ {start} earlier"),
                theme.muted_style(),
            )),
            vp.cols,
        ));
    }

    let scope_col = MAX_SCOPE.min(vp.cols / 3);
    for flat in rows.iter().skip(usize::from(start > 0)) {
        let step = flat.step;
        let mut scope = fx.path_to(step.id).join("/");
        // Left-truncate: the leaf is what identifies the row, so the ancestors go first.
        if width(&scope) > scope_col {
            let keep: String = scope
                .chars()
                .rev()
                .take(scope_col.saturating_sub(1))
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            scope = format!("…{keep}");
        }
        let elapsed = match step.status {
            Status::Pending => "pending".to_string(),
            _ => ms(step.dur_ms),
        };
        let pause = if step.id == focus { PAUSE_GLYPH } else { " " };
        let detail_col = vp
            .cols
            .saturating_sub(2 + scope_col + 1 + 2 + TIME_COL)
            .max(1);
        out.push(clip(
            Line::from(vec![
                Span::styled(
                    format!("{} ", step.status.glyph()),
                    status_style(step.status, theme),
                ),
                Span::styled(format!("{scope:<scope_col$} "), theme.tool_style()),
                Span::styled(fit(&step.detail, detail_col), Style::default()),
                Span::styled(format!(" {pause}"), theme.warn_style()),
                Span::styled(format!("{elapsed:>TIME_COL$}"), theme.muted_style()),
            ]),
            vp.cols,
        ));
    }
    tally.drew(rows.len().saturating_sub(usize::from(start > 0)));

    // The one place the thread can put context it has no column for.
    if out.len() < vp.rows.saturating_sub(tally.footer_rows()) {
        let focused = fx.focused();
        out.push(clip(
            Line::from(vec![
                Span::styled("  ⏸ pauses ", theme.muted_style()),
                Span::styled(focused.label.clone(), theme.tool_style()),
                Span::styled("  ·  ↵ opens it in a sheet", theme.muted_style()),
            ]),
            vp.cols,
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loopmock::{self, LoadCase, Mock};

    #[test]
    fn the_running_step_is_the_one_that_stays_on_screen() {
        // The whole premise of a live thread: a long run must not scroll the current step away.
        let r = loopmock::render(
            Mock::Thread,
            LoadCase::LongRun,
            loopmock::WIDE,
            &Theme::MONO,
        );
        let fx = loopmock::fixture(LoadCase::LongRun);
        assert!(
            r.to_plain().contains(&fx.focused().label),
            "the focused step scrolled off its own thread",
        );
    }

    #[test]
    fn the_scope_column_carries_the_nesting_the_layout_refuses_to_draw() {
        let r = loopmock::render(Mock::Thread, LoadCase::Tidy, loopmock::WIDE, &Theme::MONO);
        assert!(r.to_plain().contains("validate/glob"), "{}", r.to_plain());
    }
}
