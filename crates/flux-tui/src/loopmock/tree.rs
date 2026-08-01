//! Mock 2 — **the nested tree.** Indentation carries the structure, with the same `├─`/`└─`
//! connectors [`crate::plan`] already draws for a plan DAG and a pane forest.
//!
//! The bet: the thing that is hard to understand about an agent run is not the order of events but
//! *what called what* — a sub-agent's tool call and the parent's tool call look identical in a flat
//! list — so structure gets the expensive resource (horizontal space) and everything else fits
//! around it.
//!
//! It borrows the crate's existing depth bound rather than inventing one: past
//! [`crate::plan::MAX_TREE_DEPTH`] it stops descending and says how much is below.

use ratatui::text::{Line, Span};

use super::{
    clip, header, ms, shorten, status_style, width, window, Axis, Fixture, MockSpec, Status, Tally,
    Viewport, MIN_ROWS, PAUSE_GLYPH,
};
use crate::plan::MAX_TREE_DEPTH;
use crate::theme::Theme;

pub(super) const SPEC: MockSpec = MockSpec {
    name: "2 · nested tree",
    axis: Axis::Structure,
    optimizes_for: "showing what called what — a sub-agent's work is visibly *inside* the step \
                    that spawned it, and a fan-out is visibly a fan-out",
    gives_up: "width, compounding with depth: every level costs three columns forever, so the \
               deepest steps — the ones actually running — get the least room to describe \
               themselves. And once it scrolls, a step's own ancestors are what scroll away first",
    pause_affordance: "a ⏸ on the running row, where the connectors already draw the eye",
    inspection_pane: "a bottom split under the tree — the tree wants all the width it can get, so \
                      it cannot give a pane a side",
    min_cols: 44,
    // Header, footer, and enough rows that a subtree reads as a subtree rather than a fragment.
    min_rows: MIN_ROWS,
};

const TIME_COL: usize = 7;

pub(super) fn draw(
    fx: &Fixture,
    vp: Viewport,
    theme: &Theme,
    tally: &mut Tally,
) -> Vec<Line<'static>> {
    let mut out = vec![header(fx, vp, theme, "nested tree")];
    // Body rows this layout may use. The inline depth markers below come out of the same budget,
    // so the loop re-checks it rather than trusting the pre-sliced window.
    let budget = vp.rows.saturating_sub(out.len() + tally.footer_rows());

    // Only the steps this layout is willing to descend to are candidates for a row; the rest are
    // elided by depth and counted like everything else.
    let flat: Vec<_> = fx
        .flatten()
        .into_iter()
        .filter(|f| f.depth < MAX_TREE_DEPTH)
        .collect();

    let focus = fx.focused().id;
    let focus_idx = flat
        .iter()
        .position(|f| f.step.id == focus)
        .unwrap_or(flat.len().saturating_sub(1));
    let (start, rows) = window(&flat, budget, focus_idx);
    if start > 0 {
        out.push(clip(
            Line::from(Span::styled(
                format!("↑ {start} rows above — scrolled to the running step"),
                theme.muted_style(),
            )),
            vp.cols,
        ));
    }

    let mut drawn: Vec<usize> = Vec::new();
    for flat in rows.iter().skip(usize::from(start > 0)) {
        if out.len() > budget {
            break;
        }
        let step = flat.step;
        drawn.push(step.id);
        let indent = if flat.depth == 0 {
            String::new()
        } else {
            format!("{}{}", flat.guides(), if flat.last { "└─ " } else { "├─ " })
        };
        let elapsed = match step.status {
            Status::Pending => "pending".to_string(),
            _ => ms(step.dur_ms),
        };
        let pause = if step.id == focus { PAUSE_GLYPH } else { " " };
        let body = format!(
            "{} {} {} · {}",
            step.status.glyph(),
            step.kind.sigil(),
            step.label,
            step.detail
        );
        let room = vp.cols.saturating_sub(width(&indent) + 2 + TIME_COL).max(1);
        let mut spans = vec![
            Span::styled(indent, theme.muted_style()),
            Span::styled(shorten(&body, room), status_style(step.status, theme)),
        ];
        let used: usize = spans.iter().map(|s| width(&s.content)).sum();
        let gap = vp.cols.saturating_sub(used + 2 + TIME_COL);
        spans.push(Span::raw(" ".repeat(gap)));
        spans.push(Span::styled(format!(" {pause}"), theme.warn_style()));
        spans.push(Span::styled(
            format!("{elapsed:>TIME_COL$}"),
            theme.muted_style(),
        ));
        out.push(clip(Line::from(spans), vp.cols));

        // Where the depth bound bites, mark it inline as well as in the footer — the footer says
        // how many, only this says *where*.
        if flat.depth == MAX_TREE_DEPTH - 1 && !step.children.is_empty() && out.len() - 1 < budget {
            let below: usize = step.children.iter().map(count).sum();
            out.push(clip(
                Line::from(Span::styled(
                    format!(
                        "{}{}└─ … {below} deeper, past the {MAX_TREE_DEPTH}-level bound",
                        flat.guides(),
                        if flat.last { "   " } else { "│  " }
                    ),
                    theme.warn_style(),
                )),
                vp.cols,
            ));
        }
    }
    tally.drew(drawn);
    out
}

fn count(step: &super::Step) -> usize {
    1 + step.children.iter().map(count).sum::<usize>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loopmock::{self, LoadCase, Mock};

    #[test]
    fn the_depth_bound_is_marked_where_it_bites_not_only_in_the_footer() {
        let r = loopmock::render(
            Mock::Tree,
            LoadCase::DeepNesting,
            loopmock::WIDE,
            &Theme::MONO,
        );
        assert!(
            r.to_plain().contains("deeper, past the"),
            "the tree hid a subtree without marking where:\n{}",
            r.to_plain()
        );
    }

    #[test]
    fn indentation_is_the_width_tax_this_mock_exists_to_expose() {
        // Deep nesting at a narrow width: the connectors take more of the row than the content.
        // Not a bug — the finding. If this ever stops being true the comparison has changed.
        let plain = loopmock::render(
            Mock::Tree,
            LoadCase::DeepNesting,
            loopmock::NARROW,
            &Theme::MONO,
        )
        .to_plain();
        let worst = plain
            .lines()
            .filter(|l| l.contains("│"))
            .map(|l| l.chars().take_while(|c| "│├└─ ".contains(*c)).count())
            .max()
            .unwrap_or(0);
        assert!(
            worst * 2 >= loopmock::NARROW.cols / 2,
            "indentation only reached {worst} cols of {}",
            loopmock::NARROW.cols
        );
    }
}
