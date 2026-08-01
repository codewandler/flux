//! A-144: the five loop-view mocks, held to the acceptance that decides whether the comparison is
//! worth anything.
//!
//! A mock that only draws a tidy six-step flow proves nothing about the run that matters, so the
//! headline test here is the load matrix: every mock, under every hard case, at every viewport —
//! inside its bounds, and naming whatever it hides.

use flux_tui::loopmock::{self, LoadCase, Mock, Viewport, LOAD_CASES, MOCKS};
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
            render.steps_drawn + hidden <= total,
            "{} / {} / {}x{}: drew {} + hid {} of {} steps",
            mock.spec().name,
            case.name(),
            vp.cols,
            vp.rows,
            render.steps_drawn,
            hidden,
            total,
        );
        // Anything a mock does not draw it must have counted.
        if render.steps_drawn < total && !render.below_floor {
            assert_eq!(
                render.steps_drawn + hidden,
                total,
                "{} / {} / {}x{}: {} steps unaccounted for",
                mock.spec().name,
                case.name(),
                vp.cols,
                vp.rows,
                total - render.steps_drawn - hidden,
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
