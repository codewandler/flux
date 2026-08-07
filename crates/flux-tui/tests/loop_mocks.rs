//! A-144: the five loop-view mocks, held to the acceptance that decides whether the comparison is
//! worth anything.
//!
//! A mock that only draws a tidy six-step flow proves nothing about the run that matters, so the
//! headline test here is the load matrix: every mock, under every hard case, at every viewport —
//! inside its bounds, and naming whatever it hides.

use flux_tui::loopmock::{
    self, Axes, Condense, Depth, LoadCase, Mock, Shape, Viewport, AXIS_SPACE, LOAD_CASES, MOCKS,
};
use flux_tui::theme::Theme;

/// Widths the sweep visits: every mock's `min_cols` and one column under it — the boundaries where
/// a layout changes behaviour — plus a spread either side.
const COLS: [usize; 18] = [
    28, 30, 39, 40, 43, 44, 45, 46, 47, 48, 52, 63, 64, 65, 72, 80, 100, 120,
];
/// Heights the sweep visits: every row count from the lowest any mock accepts up to a tall
/// terminal. `render` accepts an unbounded `rows`, but above the tallest fixture nothing changes.
const ROWS: std::ops::RangeInclusive<usize> = 3..=30;

/// Every mock × every hard case × **the whole viewport envelope `render` accepts**, not the three
/// viewports the artifact happens to present.
///
/// The three-viewport version of this was a guard tested against its own assumptions: it agreed
/// with the sizes the snapshot set had chosen, so a property that failed at a fourth size — and one
/// did, at `rows == 6` — was invisible. The envelope is the contract; sample it densely or the
/// tests are only evidence about the pictures.
fn matrix() -> Vec<(Mock, LoadCase, Viewport)> {
    let mut out = Vec::new();
    for mock in MOCKS {
        for case in LOAD_CASES {
            for cols in COLS {
                for rows in ROWS {
                    out.push((mock, case, Viewport { cols, rows }));
                }
            }
        }
    }
    out
}

// ===========================================================================
// A-146 — the three axes
//
// The claim under test: the five mocks are not five candidates but points in a space with three
// orthogonal controls, so that "the flat thread with condensing and a depth limit and an optional
// pane *is* the split". These tests measure that rather than argue it, and they are the evidence
// behind the DEFAULTS section of `loopmock::RECOMMENDATION`.
// ===========================================================================

/// Every configuration × every hard case × the same viewport envelope the five are held to.
fn axis_matrix() -> Vec<(Axes, LoadCase, Viewport)> {
    let mut out = Vec::new();
    for axes in AXIS_SPACE {
        for case in LOAD_CASES {
            for cols in COLS {
                for rows in ROWS {
                    out.push((*axes, case, Viewport { cols, rows }));
                }
            }
        }
    }
    out
}

/// Whether the split has room to express its own rule at `vp`: **every** top-level step on the
/// rail. That is what mock 3's `rail_rows` is for — one row per top-level step, plus the focused
/// one's subtree — and below the rows it needs, its window collapses the rail onto the tail around
/// the focus, which is what *any* uncondensed view shows. Comparisons taken there measure the
/// terminal rather than the layout, which is the confound A-144 was already caught by once.
fn split_can_express_its_rule(case: LoadCase, vp: Viewport, theme: &Theme) -> bool {
    let split = loopmock::render(Mock::Split, case, vp, theme);
    !split.below_floor
        && loopmock::fixture(case)
            .steps
            .iter()
            .all(|s| split.represented.contains(&s.id))
}

/// Every configuration whose drawing of `case` at `vp` has the same [`Shape`] as mock 3's — the
/// same steps shown and the same number withheld, with *how* each draws them thrown away. That is
/// the only comparison this story can be settled on: mocks 1 and 2 prove a shape does not determine
/// a picture, and A-145 proved a withheld *count* does not determine a shape.
fn axes_matching_the_split(case: LoadCase, vp: Viewport, theme: &Theme) -> Vec<String> {
    let split = Shape::of(&loopmock::render(Mock::Split, case, vp, theme), case);
    AXIS_SPACE
        .iter()
        .filter(|a| {
            let r = loopmock::render_axes(**a, case, vp, theme);
            // A refusal notice represents no steps, so two layouts under their floors are trivially
            // "equal" at nothing — an artefact that would report agreement exactly where neither
            // draws.
            !r.below_floor && Shape::of(&r, case) == split
        })
        .map(|a| a.label())
        .collect()
}

/// ⚠⚠ **THE HEADLINE, AND IT IS A REFUTATION.**
///
/// The story's claim is that composing the three axes reproduces the five layouts, and the case it
/// names is the split: *"the flat thread with condensing on, a depth limit and the pane enabled
/// renders equivalently to the split"*. Swept over the **whole** axis space rather than the single
/// setting the claim names, it is false, and the boundary is exactly this:
///
/// > **The axes reach the split only when the run has ONE top-level step, or the terminal is too
/// > short for the split to draw its own rule. Give it nine turns and the rows to show them and
/// > nothing in the space reaches it.**
///
/// Measured over the full envelope: on the real nine-turn session there are 24 viewports where the
/// split both draws its rule and withholds something, and **zero** matches in them; on the fan-out
/// case, 42 viewports and zero matches. On one recorded *turn* every viewport matches — because
/// with a single root the split's rule and an uncondensed view are literally the same rule.
///
/// The reason is structural rather than a tuning miss. The split's rail is not "condense completed":
/// it is *one row per top-level step, plus the focused top-level step's entire subtree* — including
/// that subtree's **completed** work, which condensing by definition folds away. So its rule
/// discriminates on **focus** and condensing discriminates on **status**. With one root the two
/// coincide; with nine turns they cannot. Section 2 of the recommendation says what A-137 owes.
///
/// ⚠ An earlier reading of this measurement, taken only at 100×28, concluded that the axes agree
/// with the split "exactly when the split is hiding nothing". The full sweep falsifies it: at 64×10
/// the split withholds 10 of the tidy case's 18 steps and two configurations still match it. The
/// agreement is about **one root and too few rows**, not about hiding nothing — and the difference
/// matters, because the first phrasing would have sent A-137 looking for the divergence in the
/// elision policy instead of in the rail's rule.
#[test]
fn the_axes_reach_the_split_only_with_one_root_or_too_few_rows() {
    let theme = Theme::MONO;
    for case in LOAD_CASES {
        let fx = loopmock::fixture(case);
        // With a single top-level step there is nothing for a focus-relative rule to be relative
        // *to*, so this test has no claim to make about that case. `…coincide_on_a_single_root`
        // pins what happens there instead.
        if fx.steps.len() < 2 {
            continue;
        }
        for cols in COLS {
            for rows in ROWS {
                let vp = Viewport { cols, rows };
                if !split_can_express_its_rule(case, vp, &theme) {
                    continue;
                }
                let split = loopmock::render(Mock::Split, case, vp, &theme);
                if split.steps_drawn() == fx.step_count() {
                    continue;
                }
                // ⚠ A-137 added a third condensing setting, `TopLevel`, which *is* the rail's rule —
                // fold every non-focused top-level step, keep the focused one whole. It reaches the
                // split where the one-bit flag never could, so it is excluded here and measured on
                // its own in `top_level_condensing_reaches_the_split_only_on_a_run_without_a_failure`.
                // A-146's claim is about the three axes it built, and stays true of them.
                let matched: Vec<String> = axes_matching_the_split(case, vp, &theme)
                    .into_iter()
                    .filter(|label| !label.contains("top-level"))
                    .collect();
                assert!(
                    matched.is_empty(),
                    "{} / {cols}x{rows}: the split draws its whole rail, withholds {} of {} \
                     steps, and yet {matched:?} reproduces it — if this ever passes, section 2 of \
                     the recommendation is stale and needs rewriting, not deleting",
                    case.name(),
                    fx.step_count() - split.steps_drawn(),
                    fx.step_count(),
                );
            }
        }
    }
}

/// The other side of the boundary, pinned so the refutation above is bounded rather than absolute:
/// on a run with **one** top-level step the split's focus-relative rail and an uncondensed composed
/// view are the same rule, and they agree at every viewport where both draw.
///
/// ⚠ This is the trap A-145 built the nine-turn case to expose, caught a second time. A-144's whole
/// fixture had one root. Anyone testing "is the split a point in the axis space?" against a
/// single-turn run would have measured 60 viewports out of 60 agreeing and called the composition
/// proved.
#[test]
fn the_split_and_the_axes_coincide_on_a_single_root_run() {
    let theme = Theme::MONO;
    let case = LoadCase::Tidy;
    assert_eq!(
        loopmock::fixture(case).steps.len(),
        1,
        "the tidy case is supposed to be one recorded turn",
    );
    let mut agreed = 0usize;
    for cols in COLS {
        for rows in ROWS {
            let vp = Viewport { cols, rows };
            if !split_can_express_its_rule(case, vp, &theme) {
                continue;
            }
            if !axes_matching_the_split(case, vp, &theme).is_empty() {
                agreed += 1;
            }
        }
    }
    assert!(
        agreed > 20,
        "only {agreed} viewports agreed on a single-root run — the coincidence this test documents \
         has gone, and the refutation's scoping needs re-deriving",
    );
}

/// **What the composition does reproduce, exactly.** The nested tree is
/// `depth 6 · condense off · pane off` — 6 being `plan::MAX_TREE_DEPTH`, the bound mock 2 borrows
/// from the live plan renderer. Not "approximately": the same steps, on every load case.
#[test]
fn composing_the_axes_reproduces_the_nested_tree_exactly() {
    let theme = Theme::MONO;
    let axes = Axes {
        depth: Depth::Levels(6),
        condense: Condense::Off,
        pane: false,
    };
    for case in LOAD_CASES {
        for vp in [loopmock::WIDE, loopmock::NARROW] {
            let tree = loopmock::render(Mock::Tree, case, vp, &theme);
            let composed = loopmock::render_axes(axes, case, vp, &theme);
            assert_eq!(
                composed.represented,
                tree.represented,
                "{} / {}x{}: the tree is supposed to be a point in this space",
                case.name(),
                vp.cols,
                vp.rows,
            );
        }
    }
}

/// ⚠ **The claim's second failure, and the more interesting one: mocks 1 and 2 are the SAME point.**
///
/// The story assigns the depth limit the job of turning the flat thread into the tree. It cannot,
/// because that is not what separates them. The thread draws every step at every depth and spends no
/// column on indentation; the tree draws every step at every depth and spends three columns per
/// level. **Neither hides anything the other shows** — on all four cases the composed view at
/// `depth ∞ · condense off · pane off` has the identical step set to the flat thread, and on the
/// three cases where the tree's own depth bound never bites, the tree's is identical too.
///
/// So the thread↔tree axis is *indentation*: a drawing decision the three show/hide controls cannot
/// express, and a **fourth** control if A-137 wants both pictures. A depth limit is a real and
/// useful control — it is just not this one.
#[test]
fn the_flat_thread_and_the_nested_tree_are_one_point_in_the_axis_space() {
    let theme = Theme::MONO;
    let vp = loopmock::WIDE;
    let unlimited = Axes {
        depth: Depth::All,
        condense: Condense::Off,
        pane: false,
    };
    for case in LOAD_CASES {
        let thread = loopmock::render(Mock::Thread, case, vp, &theme);
        let composed = loopmock::render_axes(unlimited, case, vp, &theme);
        assert_eq!(
            composed.represented,
            thread.represented,
            "{}: the flat thread's view is not reachable",
            case.name(),
        );
        // The same view, and demonstrably not the same picture.
        assert_ne!(
            composed.to_plain(),
            thread.to_plain(),
            "{}: the composed view drew the flat thread's picture, so this test proves nothing",
            case.name(),
        );
    }

    // And on every case the log can actually produce — A-145 measured real nesting at three levels,
    // so the tree's six-level bound never bites — the two mocks withhold the identical set.
    for case in LOAD_CASES.iter().filter(|c| c.is_recorded()) {
        assert_eq!(
            loopmock::render(Mock::Thread, *case, vp, &theme).represented,
            loopmock::render(Mock::Tree, *case, vp, &theme).represented,
            "{}: mocks 1 and 2 stopped being the same view",
            case.name(),
        );
    }
}

/// "Real and independent": each control changes the drawing with the other two held fixed. Two
/// knobs that only did something together would be one knob with a confusing interface.
#[test]
fn each_axis_moves_the_drawing_on_its_own() {
    let theme = Theme::MONO;
    let vp = loopmock::WIDE;
    let base = Axes {
        depth: Depth::All,
        condense: Condense::Off,
        pane: false,
    };
    for case in LOAD_CASES {
        let flat = loopmock::render_axes(base, case, vp, &theme).to_plain();
        for (name, moved) in [
            (
                "depth",
                Axes {
                    depth: Depth::Levels(2),
                    ..base
                },
            ),
            (
                "condense",
                Axes {
                    condense: Condense::Uniform,
                    ..base
                },
            ),
            ("pane", Axes { pane: true, ..base }),
        ] {
            assert_ne!(
                loopmock::render_axes(moved, case, vp, &theme).to_plain(),
                flat,
                "{} / {name}: the axis is settable and changes nothing",
                case.name(),
            );
        }
    }
}

/// ⚠ **A-144's honesty property, held over three new ways to hide things.** `Tally::finish` made it
/// unconditional for the five; the point of sweeping the same envelope here is that a depth limit,
/// condensing and a pane are each a fresh opportunity to withhold something quietly.
#[test]
fn the_whole_axis_space_stays_inside_its_viewport_and_names_what_it_elides() {
    let theme = Theme::MONO;
    for (axes, case, vp) in axis_matrix() {
        let render = loopmock::render_axes(axes, case, vp, &theme);
        let where_ = format!(
            "{} / {} / {}x{}",
            axes.label(),
            case.name(),
            vp.cols,
            vp.rows
        );

        let over = render.overflowing(vp.cols);
        assert!(
            over.is_empty(),
            "{where_}: {} line(s) wider than {} cols, first {:?}",
            over.len(),
            vp.cols,
            over.first(),
        );
        assert!(
            render.lines.len() <= vp.rows,
            "{where_}: {} lines in a {}-row viewport",
            render.lines.len(),
            vp.rows,
        );
        assert!(!render.lines.is_empty(), "{where_}: drew nothing");

        let plain = render.to_plain();
        for elision in &render.elisions {
            assert!(elision.hidden > 0, "{where_}: an elision of nothing");
            assert!(
                plain.contains(&elision.marker),
                "{where_}: withheld {} {} without showing {:?}\n{plain}",
                elision.hidden,
                elision.what,
                elision.marker,
            );
        }

        // And the step accounting still derives from `total - drawn`, unbribed by a new axis.
        if !render.below_floor {
            let total = loopmock::fixture(case).step_count();
            let hidden: usize = render
                .elisions
                .iter()
                .filter(|e| e.what == loopmock::STEPS)
                .map(|e| e.hidden)
                .sum();
            assert_eq!(
                render.steps_drawn() + hidden,
                total,
                "{where_}: {} steps unaccounted for",
                total - render.steps_drawn() - hidden,
            );
        }
    }
}

/// ⚠ **Condensing must never swallow a failure**, pinned against the failure the log actually
/// recorded rather than one invented to be caught.
///
/// Session `s_1477` turn 7 ran `git_stage` on a path that no longer existed; it failed with
/// `exit 128`, and the `execute_batch` phase around it then closed **ok**. So the fixture contains
/// a `Done` parent holding a `Failed` child — which is the shape that makes "finished work collapses
/// to one row" dangerous, and it is not a shape anybody would have thought to author.
#[test]
fn condensing_never_swallows_the_recorded_failure() {
    let theme = Theme::MONO;
    let case = LoadCase::Tidy;
    let failed: Vec<usize> = loopmock::fixture(case)
        .flatten()
        .iter()
        .filter(|f| f.step.status == loopmock::Status::Failed)
        .map(|f| f.step.id)
        .collect();
    assert_eq!(
        failed.len(),
        1,
        "the recorded turn is supposed to contain exactly one real failure",
    );

    for axes in AXIS_SPACE.iter().filter(|a| a.condense != Condense::Off) {
        // Held against the same configuration with condensing off: condensing is not allowed to be
        // the reason a failure left the screen.
        let off = Axes {
            condense: Condense::Off,
            ..*axes
        };
        let with = loopmock::render_axes(*axes, case, loopmock::WIDE, &theme);
        let without = loopmock::render_axes(off, case, loopmock::WIDE, &theme);
        for id in &failed {
            assert!(
                with.represented.contains(id) || !without.represented.contains(id),
                "{}: condensing hid the failed step this fixture exists to protect",
                axes.label(),
            );
        }
    }
}

/// ⚠ **And the result that inverts the worry.** The story's fear was that condensing would flatten
/// the run by hiding a failure. Measured on the real nine-turn session the opposite happens:
/// condensing is what **buys the room** to show it.
///
/// With condensing off, the failed `git_stage` sits 166 steps back in turn 7 and the terminal's
/// window has long since scrolled past it. With condensing on, the six clean turns fold to a row
/// each, turn 7 refuses to fold *because* it holds a failure, and the failure is on screen. The
/// axis whose risk was hiding a failure is the one that reveals this one.
#[test]
fn on_a_long_run_condensing_is_what_makes_the_failure_visible() {
    let theme = Theme::MONO;
    let case = LoadCase::LongRun;
    let vp = loopmock::WIDE;
    let base = Axes {
        depth: Depth::All,
        condense: Condense::Off,
        pane: false,
    };
    let failure = "✗ → git_stage";
    assert!(
        !loopmock::render_axes(base, case, vp, &theme)
            .to_plain()
            .contains(failure),
        "the uncondensed nine-turn view already shows the failure — the finding is stale",
    );
    assert!(
        loopmock::render_axes(
            Axes {
                condense: Condense::Uniform,
                ..base
            },
            case,
            vp,
            &theme
        )
        .to_plain()
        .contains(failure),
        "condensing stopped surfacing the recorded failure on the long run",
    );
}

/// ⚠ **A depth limit must say how many LEVELS it withheld**, not merely that there is more below.
/// A sub-agent's entire run can live in one withheld level, so "some of it is deeper" is not an
/// answer a reader can act on — and where the withheld levels contain a failure, the marker says
/// that too.
#[test]
fn a_depth_limit_reports_the_number_of_levels_it_withheld() {
    let theme = Theme::MONO;
    for case in LOAD_CASES {
        for axes in AXIS_SPACE.iter().filter(|a| a.depth != Depth::All) {
            let render = loopmock::render_axes(*axes, case, loopmock::WIDE, &theme);
            let levels: Vec<_> = render
                .elisions
                .iter()
                .filter(|e| e.what == loopmock::LEVELS)
                .collect();
            let total = loopmock::fixture(case).step_count();
            if render.steps_drawn() == total {
                continue;
            }
            // Depth is not the only thing that can withhold here, so this only fires where the
            // depth limit is genuinely below the fixture's own nesting.
            let deepest = loopmock::fixture(case)
                .flatten()
                .iter()
                .map(|f| f.depth)
                .max()
                .unwrap_or(0);
            let limit = match axes.depth {
                Depth::Levels(n) => n,
                Depth::All => continue,
            };
            if deepest < limit {
                continue;
            }
            assert!(
                !levels.is_empty(),
                "{} / {}: {limit} levels drawn of {} and nothing said how many were withheld",
                axes.label(),
                case.name(),
                deepest + 1,
            );
            for elision in levels {
                assert!(
                    elision.hidden > 0 && render.to_plain().contains(&elision.marker),
                    "{} / {}: {:?} is not on screen",
                    axes.label(),
                    case.name(),
                    elision.marker,
                );
            }
        }
    }
}

/// ⚠ **The floor re-measured per configuration, which is what the story asked for.**
///
/// A-144 charged the split a 64×10 floor and called it the layout's main cost. It is the *pane's*:
/// with the pane off the composed view draws every case at 40×6 — the flat thread's floor, the
/// lowest of the five — and with it on it refuses one column under 64 exactly as mock 3 does. So
/// the sub-64-column fallback A-144 recommended stops being a second layout and becomes this layout
/// with a toggle off, which is the concrete thing making the pane optional buys.
#[test]
fn the_panes_floor_travels_with_the_pane_and_not_with_the_layout() {
    let theme = Theme::MONO;
    let with = Axes {
        pane: true,
        ..Axes::DEFAULT
    };
    let without = Axes::DEFAULT;
    assert_eq!(without.floor(), (40, 6));
    assert_eq!(with.floor(), (64, 10));

    for case in LOAD_CASES {
        // The pane's floor is the split's, at both ends of it.
        for (vp, refuses) in [
            (Viewport { cols: 64, rows: 10 }, false),
            (Viewport { cols: 63, rows: 10 }, true),
            (Viewport { cols: 64, rows: 9 }, true),
        ] {
            assert_eq!(
                loopmock::render_axes(with, case, vp, &theme).below_floor,
                refuses,
                "{}: pane on at {}x{}",
                case.name(),
                vp.cols,
                vp.rows,
            );
        }
        // And with it off, the same view draws in the terminals the pane cannot.
        for vp in [
            Viewport { cols: 40, rows: 6 },
            Viewport { cols: 52, rows: 20 },
            Viewport { cols: 63, rows: 10 },
        ] {
            assert!(
                !loopmock::render_axes(without, case, vp, &theme).below_floor,
                "{}: the pane's floor followed the layout to {}x{}",
                case.name(),
                vp.cols,
                vp.rows,
            );
        }
    }
}

/// ⚠ **Defaults are the honesty question**, so each one is pinned to what it withholds rather than
/// to a preference. Every axis defaults to the setting that hides least, and "least" means something
/// different on each of the three — which is why this checks three properties and not one ratio.
#[test]
fn every_axis_default_is_the_setting_that_withholds_least() {
    let theme = Theme::MONO;
    // The defaults themselves, so a change to them has to come through this test's reasoning.
    const { assert!(matches!(Axes::DEFAULT.depth, Depth::All)) };
    // ⚠ Uniform, not top-level. Both fold finished work, but top-level withholds every
    // non-focused turn's interior, and this test is about the setting that withholds least.
    const { assert!(matches!(Axes::DEFAULT.condense, Condense::Uniform)) };
    const { assert!(!Axes::DEFAULT.pane) };

    for case in LOAD_CASES {
        for cols in COLS {
            for rows in ROWS {
                let vp = Viewport { cols, rows };
                let render = loopmock::render_axes(Axes::DEFAULT, case, vp, &theme);
                // Depth: `All` withholds no level, anywhere in the envelope.
                assert!(
                    !render.elisions.iter().any(|e| e.what == loopmock::LEVELS),
                    "{} / {cols}x{rows}: the default depth withheld a level",
                    case.name(),
                );
                // Pane: off, so the layout draws where the pane would have refused.
                assert!(
                    !render.below_floor || cols < 40 || rows < 6,
                    "{} / {cols}x{rows}: the default refused above the no-pane floor",
                    case.name(),
                );
            }
        }
    }

    // Condensing: on, and every step it folds away is attributed to a row that says how many —
    // which is the sense in which it shows more than it hides. It relocates steps into a visible
    // summary; it does not remove them from the accounting.
    for case in LOAD_CASES {
        let render = loopmock::render_axes(Axes::DEFAULT, case, loopmock::WIDE, &theme);
        let folded: usize = render
            .elisions
            .iter()
            .filter(|e| e.what == loopmock::CONDENSED)
            .map(|e| e.hidden)
            .sum();
        let total = loopmock::fixture(case).step_count();
        assert!(
            render.steps_drawn() + folded <= total,
            "{}: condensing folded more steps than the run has",
            case.name(),
        );
    }
}

/// A-145's headline: the cases that carry the comparison are **reconstructed from a recorded run**,
/// not invented by the same context that then picks a layout from them. The proof is that they
/// contain the recorded run's own vocabulary — `detect_intent`, `explore`, `approve_batch` are the
/// agent loop's top-level ops as they appear in `~/.flux/events.db`, and no hand-authored fixture
/// contains them.
#[test]
fn the_load_bearing_cases_are_reconstructed_from_a_recorded_run() {
    let fx = loopmock::fixture(LoadCase::LongRun);
    assert!(
        fx.title.contains("s_1477"),
        "the long-run case is not the recorded session: {:?}",
        fx.title,
    );
    let labels: Vec<&str> = fx.flatten().iter().map(|f| f.step.label.as_str()).collect();
    for op in ["detect_intent", "explore", "approve_batch", "git_commit"] {
        assert!(
            labels.contains(&op),
            "the recorded run's `{op}` is missing — this is a hand-authored flow, not a capture",
        );
    }
}

/// The exact case the widened sweep was written to catch, pinned by name so a future change to
/// `min_rows` or to the pane's chrome cannot quietly reintroduce it.
///
/// At `rows == 6` the split's pane assembled six lines — breadcrumb, header, timing, rule, its own
/// `+N more` marker, the hint — while `draw` composed only four, so the marker was cut off screen
/// while `Tally` still recorded the elision. A recorded elision nobody can read is a silent
/// truncation, which is the one thing this module claims not to do.
#[test]
fn a_short_terminal_cannot_cut_the_split_panes_own_elision_marker() {
    let theme = Theme::MONO;
    for rows in 3..=12 {
        for case in LOAD_CASES {
            let vp = Viewport { cols: 100, rows };
            let render = loopmock::render(Mock::Split, case, vp, &theme);
            let plain = render.to_plain();
            for elision in &render.elisions {
                assert!(
                    plain.contains(&elision.marker),
                    "split / {} / 100x{rows}: withheld {} {} without showing {:?}\n{plain}",
                    case.name(),
                    elision.hidden,
                    elision.what,
                    elision.marker,
                );
            }
        }
    }
}

/// The module's central claim, checked across the whole envelope: a render fits its terminal, and
/// anything it withholds it says out loud.
#[test]
fn every_mock_renders_every_hard_case_within_its_viewport_and_names_what_it_elides() {
    let theme = Theme::MONO;
    for (mock, case, vp) in matrix() {
        let render = loopmock::render(mock, case, vp, &theme);
        let where_ = format!(
            "{} / {} / {}x{}",
            mock.spec().name,
            case.name(),
            vp.cols,
            vp.rows
        );

        // A mock that overflows its terminal is not a candidate; it is a bug drawn large.
        let over = render.overflowing(vp.cols);
        assert!(
            over.is_empty(),
            "{where_}: {} line(s) wider than {} cols, first: {:?}",
            over.len(),
            vp.cols,
            over.first().map(|(i, w)| (*i, *w)),
        );
        assert!(
            render.lines.len() <= vp.rows,
            "{where_}: {} lines in a {}-row viewport",
            render.lines.len(),
            vp.rows,
        );
        assert!(!render.lines.is_empty(), "{where_}: drew nothing");

        // Honest elision: whatever a render withholds, it says so on screen.
        let plain = render.to_plain();
        for elision in &render.elisions {
            assert!(
                plain.contains(&elision.marker),
                "{where_}: withheld {} {} without showing {:?}",
                elision.hidden,
                elision.what,
                elision.marker,
            );
            assert!(elision.hidden > 0, "{where_}: an elision of nothing");
        }
    }
}

#[test]
fn a_mock_that_drops_steps_accounts_for_every_one_of_them() {
    let theme = Theme::MONO;
    for (mock, case, vp) in matrix() {
        let render = loopmock::render(mock, case, vp, &theme);
        let total = loopmock::fixture(case).step_count();
        let hidden: usize = render
            .elisions
            .iter()
            .filter(|e| e.what == loopmock::STEPS)
            .map(|e| e.hidden)
            .sum();
        assert!(
            render.steps_drawn() + hidden <= total,
            "{} / {} / {}x{}: drew {} + hid {} of {} steps",
            mock.spec().name,
            case.name(),
            vp.cols,
            vp.rows,
            render.steps_drawn(),
            hidden,
            total,
        );
        // Anything a mock does not draw it must have counted.
        if render.steps_drawn() < total && !render.below_floor {
            assert_eq!(
                render.steps_drawn() + hidden,
                total,
                "{} / {} / {}x{}: {} steps unaccounted for",
                mock.spec().name,
                case.name(),
                vp.cols,
                vp.rows,
                total - render.steps_drawn() - hidden,
            );
        }
    }
}

/// ⚠ **A regression guard, not evidence.** Whether the five are *genuinely* different strategies
/// is a judgement a human makes from `docs/designs/agent-loop-visibility-mocks.md`; nothing here
/// can make it. All this checks is that five distinct `Axis` labels are still declared and that no
/// two renders became byte-identical — a one-character difference satisfies it. Read it as "nobody
/// has quietly collapsed two of the five into the same layout", and read the snapshot set for the
/// actual claim.
#[test]
fn the_five_mocks_still_declare_five_axes_and_draw_five_pictures() {
    let axes: Vec<_> = MOCKS.iter().map(|m| m.spec().axis).collect();
    for (i, a) in axes.iter().enumerate() {
        for b in &axes[i + 1..] {
            assert_ne!(a, b, "two mocks claim the same primary axis: {a:?}");
        }
    }

    let theme = Theme::MONO;
    for case in LOAD_CASES {
        let drawn: Vec<String> = MOCKS
            .iter()
            .map(|m| loopmock::render(*m, case, loopmock::WIDE, &theme).to_plain())
            .collect();
        for (i, a) in drawn.iter().enumerate() {
            for (j, b) in drawn.iter().enumerate().skip(i + 1) {
                assert_ne!(
                    a,
                    b,
                    "{} and {} draw {} identically",
                    MOCKS[i].spec().name,
                    MOCKS[j].spec().name,
                    case.name(),
                );
            }
        }
    }
}

#[test]
fn every_mock_states_its_trade_off_and_has_room_for_pause_and_inspection() {
    for mock in MOCKS {
        let spec = mock.spec();
        // "A mock with no stated trade-off is a picture, not a candidate."
        assert!(!spec.optimizes_for.is_empty(), "{}: no upside", spec.name);
        assert!(!spec.gives_up.is_empty(), "{}: no trade-off", spec.name);
        // A-140 and A-142: a layout with nowhere to put either is disqualified.
        assert!(
            !spec.pause_affordance.is_empty(),
            "{}: nowhere for A-140's pause control",
            spec.name,
        );
        assert!(
            !spec.inspection_pane.is_empty(),
            "{}: nowhere for A-142's inspection pane",
            spec.name,
        );
    }

    // And the pause affordance is drawn, not just claimed.
    let theme = Theme::MONO;
    for mock in MOCKS {
        let plain = loopmock::render(mock, LoadCase::Tidy, loopmock::WIDE, &theme).to_plain();
        assert!(
            plain.contains(loopmock::PAUSE_GLYPH),
            "{}: claims room for a pause control but never draws one",
            mock.spec().name,
        );
    }
}

/// A layout's floor is two numbers, and both are load-bearing: mock 3's pane spends four rows on
/// chrome before it says anything, so it has a row floor nearly twice the others'.
#[test]
fn below_either_floor_a_mock_says_so_instead_of_mangling_the_layout() {
    let theme = Theme::MONO;
    for mock in MOCKS {
        let spec = mock.spec();
        assert!(
            spec.min_rows >= loopmock::MIN_ROWS,
            "{}: claims a row floor below the module's own minimum",
            spec.name,
        );

        for (under, dim) in [
            (
                Viewport {
                    cols: spec.min_cols - 1,
                    rows: spec.min_rows,
                },
                "cols",
            ),
            (
                Viewport {
                    cols: spec.min_cols,
                    rows: spec.min_rows - 1,
                },
                "rows",
            ),
        ] {
            let render = loopmock::render(mock, LoadCase::Tidy, under, &theme);
            assert!(
                render.below_floor,
                "{}: drew at {}x{} without admitting it is under its {dim} floor",
                spec.name, under.cols, under.rows,
            );
            assert!(
                render.overflowing(under.cols).is_empty(),
                "{}: overflowed while under its own {dim} floor",
                spec.name,
            );
            assert!(
                render.lines.len() <= under.rows,
                "{}: overflowed rows while under its own {dim} floor",
                spec.name,
            );
        }

        // At both floors exactly it must still work, for every load case — a floor a layout cannot
        // actually draw at is a number, not a bound.
        let at = Viewport {
            cols: spec.min_cols,
            rows: spec.min_rows,
        };
        for case in LOAD_CASES {
            assert!(
                !loopmock::render(mock, case, at, &theme).below_floor,
                "{}: claims a {}x{} floor it cannot draw {} at",
                spec.name,
                spec.min_cols,
                spec.min_rows,
                case.name(),
            );
        }
    }
}

/// ⚠ **The confound in the recommendation, measured rather than asserted.**
///
/// Mock 3 wins the long-run comparison, and the recommendation credits two things at once: the
/// rail *condenses finished phases*, and the pane is a *second column*. Only the first is
/// responsible for the row count, and only mock 3 was given it — mocks 1 and 2 draw one row per
/// step at every depth by construction, so of course they scroll. This test states the size of the
/// effect so a reader can discount the causal claim: condensing alone takes the long run from
/// "does not fit any terminal here" to "fits in a third of one", before any pane exists.
#[test]
fn condensing_and_not_the_second_column_is_what_makes_the_long_run_fit() {
    let fx = loopmock::fixture(LoadCase::LongRun);
    let every_step = fx.step_count();
    // What mock 3's rail actually enumerates: top-level rows, plus the focused phase expanded.
    let focused_root = fx
        .steps
        .iter()
        .find(|s| fx.path_to(fx.focused().id).first() == Some(&s.label.as_str()))
        .expect("the focus lives under some top-level step");
    let condensed = fx.steps.len() + focused_root.children.len();

    assert!(
        every_step > loopmock::WIDE.rows,
        "the long run must not fit a wide terminal uncondensed: {every_step} steps, \
         {} rows",
        loopmock::WIDE.rows,
    );
    assert!(
        condensed * 3 <= every_step,
        "condensing is supposed to be the big effect: {condensed} rows vs {every_step} steps",
    );
    assert!(
        condensed + 2 <= loopmock::WIDE.rows,
        "the condensed rail should fit a wide terminal with room to spare: {condensed} rows",
    );
}

#[test]
fn the_graph_mock_annotates_the_renderer_a_138_will_actually_use() {
    // The gutter maps line-for-line onto `plan::render`'s output. If that renderer's shape moves,
    // this is what says so — rather than the annotations silently sliding off their nodes.
    //
    // Per case since A-145: a hand-authored fixture is handed a nine-node `plan_ast` and a matching
    // table, while a recorded one carries the one-op `plan_source` the log actually persisted and
    // derives its two-row gutter from it.
    for case in LOAD_CASES {
        assert_eq!(
            loopmock::graph_gutter_len(case),
            loopmock::graph_plan_line_count(case),
            "{}: the graph mock's status gutter no longer lines up with the plan renderer",
            case.name(),
        );
    }
}

/// "Nothing here wires live events, and nothing ships in the default TUI path."
///
/// Walks `src/` **recursively**. A non-recursive version passed today for the wrong reason —
/// `loopmock/` is currently the only subdirectory, so nothing else could have been missed — and
/// would have silently stopped covering the crate the first time any other module grew one.
#[test]
fn the_mocks_stay_out_of_the_live_tui_path() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let module = src.join("loopmock");
    let mut checked = 0usize;
    let mut stack = vec![src.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read a source directory") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                // The mocks' own files may of course name themselves.
                if path != module {
                    stack.push(path);
                }
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let rel = path
                .strip_prefix(&src)
                .unwrap_or(&path)
                .display()
                .to_string();
            let text = std::fs::read_to_string(&path).expect("read source");
            let mentions = text.matches("loopmock").count();
            checked += 1;
            if rel == "lib.rs" {
                // The module declaration is the only reference the live crate is allowed to hold.
                assert_eq!(
                    mentions, 1,
                    "lib.rs refers to loopmock {mentions} times; only the `pub mod` line may",
                );
            } else {
                assert_eq!(
                    mentions, 0,
                    "{rel} refers to loopmock — that is the live path",
                );
            }
        }
    }
    // A walk that found nothing would pass for the wrong reason.
    assert!(checked > 10, "only walked {checked} source files");
}

/// The committed side-by-side snapshot set. Regenerate with
/// `FLUX_UPDATE_GOLDEN=1 cargo test -p flux-tui --test loop_mocks` — which rewrites the file and
/// then fails on purpose (C-326), because a run that wrote a golden verified nothing.
///
/// This is the fourth checked-in-golden guard in the workspace (AGENTS.md lists them). It obeys
/// all three of C-326's rules: presence is not consent (only the exact value `1` arms it), a
/// rewrite is never reported as a check (it fails, naming the file), and an unrecognized value is
/// **refused** rather than guessed at — quietly checking would hand back a green run the author
/// read as "regenerated", and quietly rewriting would bless whatever the code currently emits on
/// the strength of a typo. `crates/flux-lang/tests/support/golden_mode.rs` is the canonical
/// statement of all three; flux-tui cannot depend on that crate's test support, so the semantics
/// are mirrored here rather than shared.
#[test]
fn the_snapshot_set_matches_the_renderers() {
    const VAR: &str = "FLUX_UPDATE_GOLDEN";
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/designs/agent-loop-visibility-mocks.md");
    let generated = loopmock::snapshot_document();

    let raw = std::env::var(VAR).ok();
    let rewrite = match raw.as_deref() {
        None | Some("") => false,
        Some("1") => true,
        Some(other) => panic!(
            "{VAR}={other} is not a value this guard recognizes.\n\
             Set `{VAR}=1` to regenerate the snapshot set, or leave it unset to check it.\n\
             Refusing to guess: checking would look like a verified run and rewriting would bless \
             whatever the code currently emits."
        ),
    };
    if rewrite {
        std::fs::write(&path, &generated).expect("write snapshot set");
        panic!("REGENERATED {}", path.display());
    }

    let committed = std::fs::read_to_string(&path).expect("read snapshot set");
    assert_eq!(
        committed, generated,
        "the committed mock snapshots are stale — regenerate with {VAR}=1",
    );
}

/// A-137's fourth decision, as a test rather than an argument.
///
/// A-146 measured that a one-bit `condense` cannot express what the owner wanted: folding uniformly
/// gives every turn's shape at one row per phase, folding only at the top level gives mock 3's rail.
/// This pins that the two settings are genuinely different drawings on a run with more than one root
/// — which is exactly the case where A-146 found the rail and condensing stop coinciding.
#[test]
fn top_level_condensing_is_a_different_drawing_from_uniform() {
    let theme = Theme::MONO;
    let uniform = Axes {
        depth: Depth::All,
        condense: Condense::Uniform,
        pane: false,
    };
    let top_level = Axes {
        condense: Condense::TopLevel,
        ..uniform
    };

    // The recorded nine-turn session. With one root the two rules coincide by construction, so a
    // multi-root case is the only place the distinction is observable.
    let many = loopmock::render_axes(uniform, LoadCase::LongRun, loopmock::WIDE, &theme);
    let rail = loopmock::render_axes(top_level, LoadCase::LongRun, loopmock::WIDE, &theme);
    assert_ne!(
        many.to_plain(),
        rail.to_plain(),
        "top-level and uniform condensing must not be the same drawing on a multi-turn run",
    );

    // And the distinction is the one claimed: top-level withholds MORE, because a finished turn's
    // whole interior folds rather than one row per phase within it.
    assert!(
        rail.steps_drawn() <= many.steps_drawn(),
        "top-level folds whole turns, so it cannot represent more steps than uniform: {} vs {}",
        rail.steps_drawn(),
        many.steps_drawn(),
    );
}

/// ⚠ **A-137's third condensing setting closes A-146's gap — but only on a run with no failure.**
///
/// A-146 measured that the split was not a point in the axis space, and named the structural reason:
/// the rail discriminates on **focus**, condensing on **status**. `Condense::TopLevel` is the rail's
/// rule written as an axis, so it reaches the split where the one-bit flag never could. Measured over
/// the same envelope:
///
/// | case | eligible viewports | reproduced by top-level | failures in the run |
/// |---|---|---|---|
/// | fan-out (hand-authored) | 42 | **36** | 0 |
/// | long run (recorded, 9 turns) | 24 | **0** | 1 |
///
/// ⚠ **The salient difference is the recorded failure, and the causal claim is NOT proven here.**
/// `condensable` refuses to fold a subtree holding a failure, so the one root that holds the real
/// `git_stage` error stays expanded while the split's rail folds it to a row — which would explain
/// zero matches. That is the leading candidate, not a measured cause: this test pins the *numbers*,
/// and anyone who needs the mechanism should isolate it rather than inherit this comment as fact.
///
/// The useful reading either way: on **recorded** load the acceptance criterion "condensing never
/// swallows a failure" and the split's rail are in tension, and the acceptance wins.
#[test]
fn top_level_condensing_reaches_the_split_only_on_a_run_without_a_failure() {
    let theme = Theme::MONO;
    for case in LOAD_CASES {
        let fx = loopmock::fixture(case);
        if fx.steps.len() < 2 {
            continue;
        }
        let failures = fx
            .flatten()
            .iter()
            .filter(|f| f.step.status == loopmock::Status::Failed)
            .count();
        let (mut eligible, mut reached) = (0usize, 0usize);
        for cols in COLS {
            for rows in ROWS {
                let vp = Viewport { cols, rows };
                if !split_can_express_its_rule(case, vp, &theme) {
                    continue;
                }
                if loopmock::render(Mock::Split, case, vp, &theme).steps_drawn() == fx.step_count()
                {
                    continue;
                }
                eligible += 1;
                if axes_matching_the_split(case, vp, &theme)
                    .iter()
                    .any(|label| label.contains("top-level"))
                {
                    reached += 1;
                }
            }
        }
        if eligible == 0 {
            continue;
        }
        if failures == 0 {
            assert!(
                reached > 0,
                "{}: with no failure to protect, top-level condensing should reach the split \
                 somewhere in {eligible} eligible viewports — it reached none",
                case.name(),
            );
        } else {
            assert_eq!(
                reached,
                0,
                "{}: this run holds {failures} recorded failure(s); if top-level condensing now \
                 reproduces the split here, the failure rule has stopped keeping that root open \
                 and THAT is the regression — check `condensable` before updating this number",
                case.name(),
            );
        }
    }
}
