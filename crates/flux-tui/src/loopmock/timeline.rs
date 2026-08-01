//! Mock 4 — **the timeline.** The x-axis is wall clock; a step's bar starts where it started and
//! is as long as it took. Overlapping bars are concurrency, and there is no other way to draw it.
//!
//! The bet: the questions an operator actually asks a live run are *where is the time going* and
//! *is anything actually running in parallel*, and both are shapes rather than text. Every other
//! layout answers them with a number in a right-hand column, which is a thing you compute rather
//! than a thing you see.
//!
//! Its limit is deliberate and structural: it draws two levels. A bar for a step nested six deep
//! would be a bar of one cell with no label, so the depth is elided and counted rather than drawn
//! badly.

use ratatui::text::{Line, Span};

use super::{
    clip, fit, header, ms, status_style, window, Axis, Fixture, MockSpec, Status, Tally, Viewport,
    PAUSE_GLYPH,
};
use crate::theme::Theme;

pub(super) const SPEC: MockSpec = MockSpec {
    name: "4 · timeline",
    axis: Axis::Time,
    optimizes_for: "where the time went and what actually overlapped — a fan-out is six bars \
                    starting at the same x, which is the only drawing here where concurrency is \
                    visible rather than inferred",
    gives_up:
        "content. A bar is not a sentence: it shows that the audit phase took 7.5s and never \
               what it found. It also flattens after two levels — a bar for a step six deep would \
               be one cell wide — and a fast run collapses into a wall of one-cell bars",
    pause_affordance: "a ⏸ in the label gutter of the running row, at the live edge of its bar",
    inspection_pane: "a bottom split: the timeline is width-hungry but short, so it can give away \
                      rows more cheaply than any other layout here",
    min_cols: 46,
    // Header, the tick axis, the footer, and at least two bars — one bar has nothing to overlap
    // with, and overlap is the whole reason to draw this.
    min_rows: 7,
};

/// Left gutter for the step label.
const LABEL: usize = 17;
/// Right column for the elapsed number the bar cannot state precisely.
const TIME_COL: usize = 7;
/// This layout draws top-level steps and their children, and no deeper.
const MAX_DEPTH: usize = 2;

pub(super) fn draw(
    fx: &Fixture,
    vp: Viewport,
    theme: &Theme,
    tally: &mut Tally,
) -> Vec<Line<'static>> {
    let mut out = vec![header(fx, vp, theme, "timeline")];
    let track = vp.cols.saturating_sub(LABEL + 2 + TIME_COL).max(8);
    let span = fx.elapsed_ms.max(1);

    out.push(clip(
        Line::from(vec![
            Span::raw(" ".repeat(LABEL + 1)),
            Span::styled(axis(span, track), theme.muted_style()),
        ]),
        vp.cols,
    ));

    let budget = vp.rows.saturating_sub(out.len() + tally.footer_rows());
    let rows: Vec<_> = fx
        .flatten()
        .into_iter()
        .filter(|f| f.depth < MAX_DEPTH)
        .collect();

    // A-140's control belongs on one row, not on every running one: the deepest step on the focus
    // path this layout is still willing to draw.
    let focus = fx.focused().id;
    let pause_id = rows
        .iter()
        .filter(|f| f.step.status == Status::Running && descends_to(f.step, focus))
        .map(|f| f.step.id)
        .next_back();

    // Windowed on the focus like the thread and the tree, deliberately: the shared helper is what
    // stops scroll policy from deciding the comparison instead of layout.
    let focus_idx = rows
        .iter()
        .position(|f| descends_to(f.step, focus))
        .unwrap_or(0);
    let (start, rows) = window(&rows, budget, focus_idx);
    if start > 0 {
        out.push(clip(
            Line::from(Span::styled(
                format!("↑ {start} earlier rows"),
                theme.muted_style(),
            )),
            vp.cols,
        ));
    }

    let mut drawn = 0usize;
    for flat in rows.iter().skip(usize::from(start > 0)) {
        let step = flat.step;
        drawn += 1;
        let label = format!(
            "{}{} {}",
            " ".repeat(flat.depth),
            step.status.glyph(),
            step.label
        );
        let elapsed = match step.status {
            Status::Pending => "pending".to_string(),
            _ => ms(step.dur_ms),
        };
        let pause = if Some(step.id) == pause_id {
            PAUSE_GLYPH
        } else {
            " "
        };
        out.push(clip(
            Line::from(vec![
                Span::styled(fit(&label, LABEL), theme.tool_style()),
                Span::styled(pause.to_string(), theme.warn_style()),
                // Padded to exactly `track` so the elapsed column stays a column rather than
                // trailing whatever the bar happened to end at.
                Span::styled(
                    fit(&bar(step, span, track), track),
                    status_style(step.status, theme),
                ),
                Span::styled(format!(" {elapsed:>TIME_COL$}"), theme.muted_style()),
            ]),
            vp.cols,
        ));
    }
    tally.drew(drawn);
    out
}

/// One step's bar: leading blanks to its start, then a body as long as it ran. A running step ends
/// in `▶` at the live edge; a pending one is a dotted stub at the far right, because a step that
/// has not started has no position on a time axis and pretending otherwise would be the flattering
/// kind of drawing this module is supposed to avoid.
fn bar(step: &super::Step, span: u64, track: usize) -> String {
    if step.status == Status::Pending {
        return format!("{}░ queued", " ".repeat(track.saturating_sub(8)));
    }
    let scale = |ms: u64| ((ms as f64 / span as f64) * track as f64).round() as usize;
    let start = scale(step.start_ms).min(track.saturating_sub(1));
    let len = scale(step.dur_ms).max(1).min(track - start);
    let head = if step.status == Status::Running {
        "▶"
    } else {
        "▏"
    };
    format!(
        "{}{}{}",
        " ".repeat(start),
        "█".repeat(len.saturating_sub(1)),
        head
    )
}

/// The tick row: a label every quarter of the track, dropping any that would collide with the one
/// before it. A narrow track showing `9.12.4s` would be a made-up number, which is worse than a
/// missing tick.
fn axis(span: u64, track: usize) -> String {
    let mut out = vec![' '; track];
    let mut end = 0usize;
    for q in 0..=4 {
        let label = ms(span * q as u64 / 4);
        let at = (track * q / 4).min(track.saturating_sub(label.chars().count()));
        if q > 0 && at < end + 2 {
            continue;
        }
        for (i, ch) in label.chars().enumerate() {
            if at + i < track {
                out[at + i] = ch;
            }
        }
        end = at + label.chars().count();
    }
    out.into_iter().collect()
}

/// Whether `id` is `step` or one of its descendants.
fn descends_to(step: &super::Step, id: usize) -> bool {
    step.id == id || step.children.iter().any(|c| descends_to(c, id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loopmock::{self, LoadCase, Mock};

    #[test]
    fn concurrent_workers_start_at_the_same_column() {
        // The one thing this layout is for. If the fan-out's bars do not line up, it has no case.
        let plain = loopmock::render(
            Mock::Timeline,
            LoadCase::FanOut,
            loopmock::WIDE,
            &Theme::MONO,
        )
        .to_plain();
        let starts: Vec<usize> = plain
            .lines()
            .filter(|l| l.contains("tracker-audit"))
            .map(|l| l.chars().take_while(|c| *c != '█').count())
            .collect();
        assert!(starts.len() >= 5, "only {} workers drawn", starts.len());
        let spread = starts.iter().max().unwrap() - starts.iter().min().unwrap();
        assert!(spread <= 2, "concurrent bars are {spread} cols apart");
    }

    #[test]
    fn a_pending_step_is_not_given_a_position_it_does_not_have() {
        let plain = loopmock::render(Mock::Timeline, LoadCase::Tidy, loopmock::WIDE, &Theme::MONO)
            .to_plain();
        assert!(plain.contains("queued"), "{plain}");
    }
}
