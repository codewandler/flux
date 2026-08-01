//! A-144: the five loop-view mocks, held to the acceptance that decides whether the comparison is
//! worth anything.
//!
//! A mock that only draws a tidy six-step flow proves nothing about the run that matters, so the
//! headline test here is the load matrix: every mock, under every hard case, at every viewport —
//! inside its bounds, and naming whatever it hides.

use flux_tui::loopmock::{self, LoadCase, Mock, Viewport, LOAD_CASES, MOCKS};
use flux_tui::theme::Theme;

/// Every mock × every hard case × every viewport — the comparison's whole matrix.
fn matrix() -> Vec<(Mock, LoadCase, Viewport)> {
    let mut out = Vec::new();
    for mock in MOCKS {
        for case in LOAD_CASES {
            for vp in [loopmock::WIDE, loopmock::NARROW, loopmock::FLOOR] {
                out.push((mock, case, vp));
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
    let labels: Vec<&str> = fx
        .flatten()
        .iter()
        .map(|f| f.step.label.as_str())
        .collect();
    for op in ["detect_intent", "explore", "approve_batch", "git_commit"] {
        assert!(
            labels.contains(&op),
            "the recorded run's `{op}` is missing — this is a hand-authored flow, not a capture",
        );
    }
}

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

#[test]
fn the_five_mocks_are_five_strategies_not_five_variations() {
    // The mechanical proxy for "genuinely different": each declares a different primary visual
    // axis, and no two draw the same picture of the same fixture.
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

#[test]
fn below_its_floor_a_mock_says_so_instead_of_mangling_the_layout() {
    let theme = Theme::MONO;
    for mock in MOCKS {
        let spec = mock.spec();
        let under = Viewport {
            cols: spec.min_cols - 1,
            rows: 14,
        };
        let render = loopmock::render(mock, LoadCase::Tidy, under, &theme);
        assert!(
            render.below_floor,
            "{}: drew a {}-col layout at {} cols without admitting it is under its floor",
            spec.name, spec.min_cols, under.cols,
        );
        assert!(
            render.overflowing(under.cols).is_empty(),
            "{}: overflowed while under its own floor",
            spec.name,
        );

        // At its floor it must still work.
        let at = Viewport {
            cols: spec.min_cols,
            rows: 14,
        };
        assert!(
            !loopmock::render(mock, LoadCase::Tidy, at, &theme).below_floor,
            "{}: claims a floor of {} cols it cannot actually draw at",
            spec.name,
            spec.min_cols,
        );
    }
}

#[test]
fn the_graph_mock_annotates_the_renderer_a_138_will_actually_use() {
    // The gutter maps line-for-line onto `plan::render`'s output. If that renderer's shape moves,
    // this is what says so — rather than the annotations silently sliding off their nodes.
    assert_eq!(
        loopmock::graph_gutter_len(),
        loopmock::graph_plan_line_count(),
        "the graph mock's status gutter no longer lines up with the plan renderer",
    );
}

#[test]
fn the_mocks_stay_out_of_the_live_tui_path() {
    // "Nothing here wires live events, and nothing ships in the default TUI path."
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for entry in std::fs::read_dir(&src).expect("read src") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let text = std::fs::read_to_string(&path).expect("read source");
        let mentions = text.matches("loopmock").count();
        match name {
            // The module declaration is the only reference the live crate is allowed to hold.
            "lib.rs" => assert_eq!(
                mentions, 1,
                "lib.rs refers to loopmock {mentions} times; only the `pub mod` line may",
            ),
            _ => assert_eq!(
                mentions, 0,
                "{name} refers to loopmock — that is the live path"
            ),
        }
    }
}

/// The committed side-by-side snapshot set. Regenerate with
/// `FLUX_UPDATE_GOLDEN=1 cargo test -p flux-tui --test loop_mocks` — which rewrites the file and
/// then fails on purpose (C-326), because a run that wrote a golden verified nothing.
#[test]
fn the_snapshot_set_matches_the_renderers() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/designs/agent-loop-visibility-mocks.md");
    let generated = loopmock::snapshot_document();
    if std::env::var("FLUX_UPDATE_GOLDEN").as_deref() == Ok("1") {
        std::fs::write(&path, &generated).expect("write snapshot set");
        panic!("REGENERATED {}", path.display());
    }
    let committed = std::fs::read_to_string(&path).expect("read snapshot set");
    assert_eq!(
        committed, generated,
        "the committed mock snapshots are stale — regenerate with FLUX_UPDATE_GOLDEN=1",
    );
}
