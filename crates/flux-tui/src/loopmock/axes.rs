//! A-146 — **the five mocks as three knobs**, so A-137 chooses defaults instead of adopting a
//! picture.
//!
//! The owner's reading of A-144's rendered set was that the five are not five candidates but points
//! in a space with three orthogonal controls:
//!
//! | axis | what it does | which mock it was claimed to become |
//! |---|---|---|
//! | [`Depth`] | how many nesting levels are drawn | flat thread ↔ tree |
//! | [`Axes::condense`] | finished work collapses to one row | the long-run win |
//! | [`Axes::pane`] | an optional detail pane | tree ↔ split |
//!
//! This module implements the three and [`render_axes`] draws their composition, so the claim can
//! be **measured** rather than argued. What the measurement says is in [`super::RECOMMENDATION`]
//! and the short version is that it half holds: the tree is reproduced exactly, the thread is
//! reproduced as a *view* but not as a *picture*, and **the split is not a point in this space at
//! all**. See `crates/flux-tui/tests/loop_mocks.rs` for the tests that pin each of those.
//!
//! ## The two rules the mocks obey, under three new ways to hide things
//!
//! Every axis here is a way to withhold something, which is exactly what A-144's honesty property
//! exists to police, so none of them gets to be an exception:
//!
//! - **Condensing** never collapses a subtree holding a failure ([`condensable`]), never collapses
//!   the path to what is running, and says how many steps it folded — per row, and once in total.
//! - **A depth limit** says how many *levels* it withheld, not merely that it withheld something:
//!   "there is more below" is not an answer a reader can act on, and a sub-agent's entire run can
//!   live in one withheld level.
//! - Both feed [`super::Tally`], so the step footer derives `total - drawn` the same way it does
//!   for the five and cannot be talked out of it by a new axis.

use std::collections::BTreeSet;

use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use super::{
    below_floor, clip, header, ms, pad, shorten, split, status_style, width, window, Fixture, Flat,
    LoadCase, Render, Status, Step, Tally, Viewport, CONDENSED, LEVELS, MIN_ROWS, PAUSE_GLYPH,
};
use crate::theme::Theme;

/// How many nesting levels the view draws.
///
/// ⚠ A-145 measured real nesting at **three** levels (turn > loop phase > op); the eight-level case
/// that cost the tree A-144's comparison is a shape this log has never recorded. So the interesting
/// settings are small, and [`Axes::DEFAULT`] argues for not limiting at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    /// Every level. The axis switched off — and the default, on the evidence.
    All,
    /// Draw `n` levels and say what is below. `Levels(1)` is top-level rows only.
    Levels(usize),
}

impl Depth {
    /// Whether a step at `depth` (0-based) is drawn.
    fn draws(self, depth: usize) -> bool {
        match self {
            Depth::All => true,
            Depth::Levels(n) => depth < n,
        }
    }

    fn label(self) -> String {
        match self {
            Depth::All => "depth ∞".to_string(),
            Depth::Levels(n) => format!("depth {n}"),
        }
    }

    /// One level further down, `Levels(8)` → [`Depth::All`]. The explorer's `]` key.
    pub fn deeper(self) -> Depth {
        match self {
            Depth::All => Depth::All,
            Depth::Levels(n) if n >= MAX_EXPLORED_DEPTH => Depth::All,
            Depth::Levels(n) => Depth::Levels(n + 1),
        }
    }

    /// One level shallower, [`Depth::All`] → `Levels(8)`, never below 1. The explorer's `[` key.
    pub fn shallower(self) -> Depth {
        match self {
            Depth::All => Depth::Levels(MAX_EXPLORED_DEPTH),
            Depth::Levels(n) => Depth::Levels(n.saturating_sub(1).max(1)),
        }
    }
}

/// Where the explorer's depth key stops counting and switches to [`Depth::All`] — one past the
/// deepest hand-authored case, and five past anything A-145 found in the log.
const MAX_EXPLORED_DEPTH: usize = 8;

/// One point in the space: a setting for each of the three controls.
///
/// The three are genuinely independent — every one of the [`AXIS_SPACE`] combinations renders, and
/// `each_axis_moves_the_drawing_on_its_own` checks that each one changes the drawing with the other two held
/// fixed, which is what "orthogonal" has to mean if it is to mean anything testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Axes {
    pub depth: Depth,
    /// Finished, wholly successful work collapses to its own row.
    pub condense: bool,
    /// The detail pane for the step in focus. **Optional** — that is the point of the axis, and
    /// [`Axes::floor`] is where it shows up as a cost.
    pub pane: bool,
}

impl Axes {
    /// **The recommendation A-146 exists to produce.** Every axis defaults to showing more than it
    /// hides; the reasoning, and what each default shows and hides, is in [`super::RECOMMENDATION`].
    pub const DEFAULT: Axes = Axes {
        depth: Depth::All,
        condense: true,
        pane: false,
    };

    /// This configuration's name, drawn in the header so a snapshot says which point it is.
    pub fn label(self) -> String {
        format!(
            "{} · condense {} · pane {}",
            self.depth.label(),
            if self.condense { "on" } else { "off" },
            if self.pane { "on" } else { "off" },
        )
    }

    /// The floor for *this* configuration, as `(cols, rows)`.
    ///
    /// ⚠ **The measurement the story asked for rather than the assumption.** A-144 charged the
    /// split a 64×10 floor — nearly double the others', and its single biggest cost. That floor
    /// belongs to the *pane*, not to the layout: with the pane off this view draws at the flat
    /// thread's 40×6, and `the_panes_floor_travels_with_the_pane_and_not_with_the_layout` confirms it at both ends rather
    /// than taking it on trust. So the sub-64-column fallback stops being a second layout and
    /// becomes the same layout with a toggle off.
    ///
    /// A depth limit and condensing move neither number: both only ever *remove* rows.
    pub fn floor(self) -> (usize, usize) {
        if self.pane {
            (split::MIN_COLS, split::MIN_ROWS)
        } else {
            (NO_PANE_MIN_COLS, MIN_ROWS)
        }
    }
}

/// Width floor with the pane off — the flat thread's, because that is what this becomes.
const NO_PANE_MIN_COLS: usize = 40;

/// The configurations the sweep, the tests and the snapshot set all visit.
///
/// Deliberately small and deliberately spanning: both settings of both toggles, and depth at `All`,
/// at the real run's three levels, and at `1` (the most aggressive setting the axis has). A sweep
/// over every depth from 1 to 8 would cost the test matrix a great deal to sample a range A-145
/// showed the log has never produced.
pub const AXIS_SPACE: &[Axes] = &[
    Axes {
        depth: Depth::All,
        condense: false,
        pane: false,
    },
    Axes {
        depth: Depth::All,
        condense: false,
        pane: true,
    },
    Axes {
        depth: Depth::All,
        condense: true,
        pane: false,
    },
    Axes {
        depth: Depth::All,
        condense: true,
        pane: true,
    },
    Axes {
        depth: Depth::Levels(3),
        condense: false,
        pane: false,
    },
    Axes {
        depth: Depth::Levels(3),
        condense: false,
        pane: true,
    },
    Axes {
        depth: Depth::Levels(3),
        condense: true,
        pane: false,
    },
    Axes {
        depth: Depth::Levels(3),
        condense: true,
        pane: true,
    },
    Axes {
        depth: Depth::Levels(1),
        condense: false,
        pane: false,
    },
    Axes {
        depth: Depth::Levels(1),
        condense: false,
        pane: true,
    },
    Axes {
        depth: Depth::Levels(1),
        condense: true,
        pane: false,
    },
    Axes {
        depth: Depth::Levels(1),
        condense: true,
        pane: true,
    },
];

/// What a drawing **shows and withholds**, with how it draws it thrown away.
///
/// The whole A-146 comparison needs this and needs it to be about identities. Two renders can have
/// the same shape and be different pictures — the flat thread and the nested tree draw the same
/// steps and look nothing alike — and two renders can look alike and have different shapes, which
/// is what the split's rail and a condensed thread turn out to be. A comparison done on row counts
/// would have got both of those backwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shape {
    /// Which of the fixture's steps reached the screen.
    pub shown: BTreeSet<usize>,
    /// How many did not.
    pub withheld: usize,
}

impl Shape {
    /// The shape of a drawing of `case`.
    pub fn of(render: &Render, case: LoadCase) -> Shape {
        Shape {
            shown: render.represented.clone(),
            withheld: super::fixture(case)
                .step_count()
                .saturating_sub(render.represented.len()),
        }
    }
}

/// Draw the composed view at one point in the axis space.
pub fn render_axes(axes: Axes, case: LoadCase, vp: Viewport, theme: &Theme) -> Render {
    let (min_cols, min_rows) = axes.floor();
    if vp.cols < min_cols || vp.rows < min_rows {
        return below_floor(&axes.label(), min_cols, min_rows, vp, theme);
    }
    let fx = super::fixture(case);
    let mut tally = Tally::new(fx.step_count());
    let body = draw(&fx, axes, vp, theme, &mut tally);
    tally.finish(body, vp, theme)
}

// ---------------------------------------------------------------------------
// The row plan — what the axes decide, before any viewport is involved
// ---------------------------------------------------------------------------

/// One planned row: either a step, or a marker saying where the depth limit stopped.
///
/// The plan is built **before** windowing, and that ordering is load-bearing: what the axes withhold
/// does not depend on the terminal, so the totals the footer notes report are the same at every
/// viewport. A plan built after windowing would report a different amount of condensing at every
/// terminal size, which is a number nobody could check against anything.
enum Row<'a> {
    Step {
        flat: Flat<'a>,
        /// Steps folded into this row by condensing — its subtree size less itself, or 0.
        condensed: usize,
    },
    /// Where a depth limit stopped descending.
    Cut {
        /// Guide columns of the step whose children were withheld.
        guides: String,
        last: bool,
        /// How many *levels* are below. The number the acceptance is about.
        levels: usize,
        steps: usize,
        failures: usize,
    },
}

impl Row<'_> {
    fn id(&self) -> Option<usize> {
        match self {
            Row::Step { flat, .. } => Some(flat.step.id),
            Row::Cut { .. } => None,
        }
    }
}

/// What the axes withheld, in totals — computed once from the plan.
#[derive(Default)]
struct Withheld {
    condensed: usize,
    /// Steps below a depth limit.
    deeper: usize,
    /// Deepest number of levels any one cut withheld.
    levels: usize,
    /// Failed steps a depth limit put out of sight. Condensing can never contribute here.
    hidden_failures: usize,
}

fn plan<'a>(fx: &'a Fixture, axes: Axes) -> (Vec<Row<'a>>, Withheld) {
    let focus_path = path_ids(fx);
    let mut rows = Vec::new();
    let mut hid = Withheld::default();
    walk(&fx.steps, 0, 0, axes, &focus_path, &mut rows, &mut hid);
    (rows, hid)
}

#[allow(clippy::too_many_arguments)]
fn walk<'a>(
    steps: &'a [Step],
    depth: usize,
    trail: u64,
    axes: Axes,
    focus_path: &BTreeSet<usize>,
    rows: &mut Vec<Row<'a>>,
    hid: &mut Withheld,
) {
    for (i, step) in steps.iter().enumerate() {
        let last = i + 1 == steps.len();
        let flat = Flat {
            step,
            depth,
            last,
            trail,
        };

        // Condensing first: it is a statement about what *happened*, and a depth limit is a
        // statement about how much of the structure to draw. A subtree already folded away has no
        // depth left to limit.
        let fold = axes.condense && !focus_path.contains(&step.id) && condensable(step);
        if fold {
            let condensed = count(step) - 1;
            hid.condensed += condensed;
            rows.push(Row::Step { flat, condensed });
            continue;
        }
        rows.push(Row::Step { flat, condensed: 0 });

        if step.children.is_empty() {
            continue;
        }
        if axes.depth.draws(depth + 1) {
            let child_trail = if last {
                trail | (1u64 << depth.min(63))
            } else {
                trail
            };
            walk(
                &step.children,
                depth + 1,
                child_trail,
                axes,
                focus_path,
                rows,
                hid,
            );
            continue;
        }
        // The depth limit bites here. Say how many levels — not just that there are some.
        let steps = count(step) - 1;
        let failures = failures_below(step);
        hid.deeper += steps;
        hid.levels = hid.levels.max(levels_below(step));
        hid.hidden_failures += failures;
        rows.push(Row::Cut {
            guides: flat.guides(),
            last,
            levels: levels_below(step),
            steps,
            failures,
        });
    }
}

/// Whether a subtree may be folded into one row: **entirely finished and entirely successful.**
///
/// ⚠ The second half is not implied by the first, and it is the half that matters. A `Done` phase
/// can hold a `Failed` op: the recorded session has exactly one — a `git_stage` that failed inside
/// an `execute_batch` that then closed `ok` — and folding that phase into a tidy one-line summary
/// would make the view flatter than the run, in precisely the case a reader most needs. It is
/// stated separately from [`finished`] so that a later change to what "finished" means cannot take
/// the failure rule with it silently.
fn condensable(step: &Step) -> bool {
    finished(step) && !holds_a_failure(step)
}

/// Nothing in this subtree is still to come or still going.
fn finished(step: &Step) -> bool {
    step.status == Status::Done && step.children.iter().all(finished)
}

fn holds_a_failure(step: &Step) -> bool {
    step.status == Status::Failed || step.children.iter().any(holds_a_failure)
}

/// Failed steps among `step`'s **descendants**. The step itself is still drawn where a depth limit
/// bites, so it is not one of the things withheld.
fn failures_below(step: &Step) -> usize {
    step.children.iter().map(failures_in_subtree).sum()
}

fn failures_in_subtree(step: &Step) -> usize {
    usize::from(step.status == Status::Failed)
        + step.children.iter().map(failures_in_subtree).sum::<usize>()
}

/// This step and every descendant.
fn count(step: &Step) -> usize {
    1 + step.children.iter().map(count).sum::<usize>()
}

/// How many levels of nesting sit below `step`.
fn levels_below(step: &Step) -> usize {
    step.children
        .iter()
        .map(|c| 1 + levels_below(c))
        .max()
        .unwrap_or(0)
}

/// The focused step and its ancestors — never condensed, whatever their status. What is running,
/// and how the run got there, is the one thing a live view exists to show.
fn path_ids(fx: &Fixture) -> BTreeSet<usize> {
    let focus = fx.focused().id;
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
    walk(&fx.steps, focus, &mut trail);
    trail.into_iter().collect()
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

/// Right-hand elapsed column, as the thread and the tree use.
const TIME_COL: usize = 7;

/// `1 level` / `3 levels`. The elision markers are the part of this artifact a reviewer actually
/// reads, and "1 levels" reads as a bug in the thing doing the counting.
fn plural(n: usize, one: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {one}s")
    }
}

fn draw(
    fx: &Fixture,
    axes: Axes,
    vp: Viewport,
    theme: &Theme,
    tally: &mut Tally,
) -> Vec<Line<'static>> {
    let mut out = vec![header(fx, vp, theme, &axes.label())];
    let (rows, hid) = plan(fx, axes);

    // The two axis notes are known from the plan, so they can be paid for before anything competes
    // for the rows — the same discipline `Tally` applies to the step footer.
    let notes = usize::from(hid.condensed > 0) + usize::from(hid.deeper > 0);
    let budget = vp
        .rows
        .saturating_sub(out.len() + tally.footer_rows() + notes);

    let (rail_cols, pane_cols) = if axes.pane {
        let rail = split::rail_cols(vp.cols);
        (rail, vp.cols.saturating_sub(rail + 3))
    } else {
        (vp.cols, 0)
    };

    let focused = fx.focused();
    let mut seen: BTreeSet<usize> = BTreeSet::new();
    let left = rail(&rows, focused.id, rail_cols, budget, theme, &mut seen);

    if axes.pane {
        let (right, hidden_detail) =
            split::pane_rows(fx, focused, pane_cols, budget, theme, &mut seen);
        let height = budget.min(left.len().max(right.len()));
        for row in 0..height {
            let l = left.get(row).cloned().unwrap_or_default();
            let r = right.get(row).cloned().unwrap_or_default();
            let mut spans = pad(l, rail_cols).spans;
            spans.push(Span::styled(" │ ", theme.muted_style()));
            spans.extend(r.spans);
            out.push(clip(Line::from(spans), vp.cols));
        }
        if hidden_detail > 0 {
            tally.hid(
                super::DETAIL,
                hidden_detail,
                format!("+{hidden_detail} more"),
            );
        }
    } else {
        out.extend(left.into_iter().take(budget));
    }

    tally.drew(seen.iter().copied());

    // ⚠ Each note is built through `shorten` and the *drawn* string is what gets registered as the
    // elision marker. Registering the untruncated text instead would leave `Tally::finish` forcing
    // a marker back onto a screen that already carries a clipped version of it — the honesty net
    // firing on a render that was in fact honest, and displacing a row to do it.
    for (what, hidden, text) in [
        (
            CONDENSED,
            hid.condensed,
            format!(
                "⊕ condensed {} into {}",
                plural(hid.condensed, "finished step"),
                plural(
                    rows.iter()
                        .filter(|r| matches!(r, Row::Step { condensed, .. } if *condensed > 0))
                        .count(),
                    "row"
                ),
            ),
        ),
        (
            LEVELS,
            hid.levels,
            format!(
                "⇣ {} below the depth limit withheld — {}{}",
                plural(hid.levels, "level"),
                plural(hid.deeper, "step"),
                match hid.hidden_failures {
                    0 => String::new(),
                    n => format!(", {n} of them failed"),
                },
            ),
        ),
    ] {
        if hidden == 0 {
            continue;
        }
        let drawn = shorten(&text, vp.cols);
        tally.hid(what, hidden, drawn.clone());
        out.push(Line::from(Span::styled(drawn, theme.warn_style())));
    }
    out
}

/// The rail: the planned rows, windowed onto the focus.
fn rail(
    rows: &[Row<'_>],
    focus: usize,
    cols: usize,
    budget: usize,
    theme: &Theme,
    seen: &mut BTreeSet<usize>,
) -> Vec<Line<'static>> {
    let focus_idx = rows.iter().position(|r| r.id() == Some(focus)).unwrap_or(0);
    let (start, rows) = window(rows, budget, focus_idx);

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
    for row in rows.iter().skip(usize::from(start > 0)) {
        out.push(match row {
            Row::Step { flat, condensed } => {
                seen.insert(flat.step.id);
                step_row(flat, *condensed, focus, cols, theme)
            }
            Row::Cut {
                guides,
                last,
                levels,
                steps,
                failures,
            } => clip(
                Line::from(Span::styled(
                    shorten(
                        &format!(
                            "{guides}{}└─ ⇣ {}, {}{}",
                            if *last { "   " } else { "│  " },
                            plural(*levels, "level"),
                            plural(*steps, "step"),
                            match failures {
                                0 => String::new(),
                                n => format!(", {n} failed"),
                            },
                        ),
                        cols,
                    ),
                    theme.warn_style(),
                )),
                cols,
            ),
        });
    }
    out
}

/// One step's row: the tree's connectors, the thread's right-hand elapsed column, and — where
/// condensing folded a subtree away — what it folded, in the fixed right-hand furniture rather
/// than at the end of a label that width pressure eats first.
fn step_row(
    flat: &Flat<'_>,
    condensed: usize,
    focus: usize,
    cols: usize,
    theme: &Theme,
) -> Line<'static> {
    let step = flat.step;
    let indent = if flat.depth == 0 {
        String::new()
    } else {
        format!("{}{}", flat.guides(), if flat.last { "└─ " } else { "├─ " })
    };
    let elapsed = match step.status {
        Status::Pending => "pending".to_string(),
        _ => ms(step.dur_ms),
    };
    let fold = if condensed > 0 {
        format!("+{condensed} ")
    } else {
        String::new()
    };
    let pause = if step.id == focus { PAUSE_GLYPH } else { " " };
    let body = if step.detail.is_empty() {
        format!(
            "{} {} {}",
            step.status.glyph(),
            step.kind.sigil(),
            step.label
        )
    } else {
        format!(
            "{} {} {} · {}",
            step.status.glyph(),
            step.kind.sigil(),
            step.label,
            step.detail
        )
    };
    let right = width(&fold) + 2 + TIME_COL;
    let room = cols.saturating_sub(width(&indent) + right).max(1);
    let mut spans = vec![
        Span::styled(indent, theme.muted_style()),
        Span::styled(
            shorten(&body, room),
            if step.id == focus {
                status_style(step.status, theme).add_modifier(Modifier::BOLD)
            } else {
                status_style(step.status, theme)
            },
        ),
    ];
    let used: usize = spans.iter().map(|s| width(&s.content)).sum();
    spans.push(Span::raw(" ".repeat(cols.saturating_sub(used + right))));
    spans.push(Span::styled(fold, theme.warn_style()));
    spans.push(Span::styled(format!(" {pause}"), theme.warn_style()));
    spans.push(Span::styled(
        format!("{elapsed:>TIME_COL$}"),
        theme.muted_style(),
    ));
    clip(Line::from(spans), cols)
}
