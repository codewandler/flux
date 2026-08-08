//! C-643 acceptance tests. Every one drives the real state machine and the real renderer through
//! `TestBackend` — there is no terminal, no clock and no registry in here.

use super::*;
use flux_spec::{Effect, Idempotency, Risk};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

fn param(name: &str, required: bool) -> ParamRow {
    ParamRow {
        name: name.into(),
        ty: "string".into(),
        description: format!("the {name} to use. More prose after the first sentence."),
        required,
    }
}

fn row(name: &str, category: &str, risk: Risk) -> OpRow {
    OpRow {
        name: name.into(),
        description: format!("Do {name} things. A second sentence that the list must not show."),
        params: vec![param("path", true), param("limit", false)],
        effects: vec![Effect::Read],
        risk,
        idempotency: Idempotency::Idempotent,
        group: None,
        category: category.into(),
        source: "builtins".into(),
        doc_public_url: format!("https://codewandler.github.io/flux/docs/language/ops#{name}"),
        doc_local_url: format!("http://127.0.0.1:8788/flux/docs/language/ops#{name}"),
    }
}

fn fixture() -> Vec<OpRow> {
    vec![
        row("git_commit", "git", Risk::Medium),
        row("git_status", "git", Risk::Low),
        row("read", "files", Risk::Low),
        row("write", "files", Risk::High),
        row("web_fetch", "web", Risk::Medium),
        row("shell", "shell", Risk::Destructive),
    ]
}

fn state() -> ExplorerState {
    ExplorerState::new(
        fixture(),
        OpsExplorerOptions {
            theme: Theme::DARK,
            seed: 0xC643,
        },
    )
}

fn press(state: &mut ExplorerState, code: KeyCode) -> Action {
    state.on_key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn ctrl(state: &mut ExplorerState, c: char) -> Action {
    state.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
}

fn draw(state: &mut ExplorerState, w: u16, h: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| render(f, state)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| {
                    buffer
                        .cell((x, y))
                        .map_or(' ', |c| c.symbol().chars().next().unwrap_or(' '))
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The empty query keeps registry order; a query ranks by the crate's existing fuzzy ranker, and
/// a non-matching query yields nothing rather than silently falling back to everything.
#[test]
fn filters_and_ranks_by_fuzzy_query() {
    let mut s = state();
    assert_eq!(
        s.filtered().len(),
        6,
        "the empty query lists every op in registry order"
    );
    assert_eq!(s.rows[s.filtered()[0]].name, "git_commit");

    for c in "git".chars() {
        press(&mut s, KeyCode::Char(c));
    }
    let names: Vec<&str> = s
        .filtered()
        .iter()
        .map(|&i| s.rows[i].name.as_str())
        .collect();
    assert_eq!(
        names,
        vec!["git_commit", "git_status"],
        "a prefix query keeps only the prefix matches"
    );
    assert_eq!(s.stage, Stage::Results, "typing moves off the start screen");

    // A subsequence match still ranks, behind the prefix hits — that is the ranker's contract and
    // this pins that the explorer actually routes through it.
    s.query.clear();
    for c in "wt".chars() {
        press(&mut s, KeyCode::Char(c));
    }
    let names: Vec<&str> = s
        .filtered()
        .iter()
        .map(|&i| s.rows[i].name.as_str())
        .collect();
    assert!(
        names.contains(&"write"),
        "subsequence query missed `write`: {names:?}"
    );

    s.query.clear();
    for c in "zzzz".chars() {
        press(&mut s, KeyCode::Char(c));
    }
    assert!(
        s.filtered().is_empty(),
        "a non-matching query matches nothing"
    );
    assert!(s.selected_row().is_none());
}

/// Categories come from the rows, cycle in both directions, wrap, and actually narrow the list.
#[test]
fn category_cycle_derives_and_filters() {
    let mut s = state();
    assert_eq!(
        s.categories,
        vec!["all", "files", "git", "shell", "web"],
        "categories are derived from the rows and sorted, with `all` pinned first"
    );
    assert_eq!(s.active_category(), ALL_CATEGORIES);

    press(&mut s, KeyCode::Tab);
    assert_eq!(s.active_category(), "files");
    let names: Vec<&str> = s
        .filtered()
        .iter()
        .map(|&i| s.rows[i].name.as_str())
        .collect();
    assert_eq!(names, vec!["read", "write"], "the filter narrows the list");

    press(&mut s, KeyCode::BackTab);
    assert_eq!(s.active_category(), ALL_CATEGORIES, "BackTab walks back");

    // Wrapping backwards from `all` lands on the last category, not out of bounds.
    press(&mut s, KeyCode::Left);
    assert_eq!(s.active_category(), "web");
    press(&mut s, KeyCode::Right);
    assert_eq!(s.active_category(), ALL_CATEGORIES);

    // The filter composes with the query rather than replacing it.
    for c in "git".chars() {
        press(&mut s, KeyCode::Char(c));
    }
    press(&mut s, KeyCode::Tab); // -> files
    assert!(
        s.filtered().is_empty(),
        "category `files` and query `git` intersect to nothing"
    );
}

/// The two-focus model, exactly as the story specifies it. This is the test that stops a future
/// edit from making `q` quit while someone is typing a query.
#[test]
fn key_table_focus_model() {
    let mut s = state();

    // Typing always edits the query — including the command letters.
    for c in "qjky?".chars() {
        press(&mut s, KeyCode::Char(c));
    }
    assert_eq!(
        s.query, "qjky?",
        "command letters are query text in input focus"
    );
    assert_eq!(s.focus, Focus::Input);

    // Enter is the only door into command focus.
    s.query.clear();
    for c in "git".chars() {
        press(&mut s, KeyCode::Char(c));
    }
    press(&mut s, KeyCode::Enter);
    assert_eq!(s.focus, Focus::Command);

    press(&mut s, KeyCode::Char('j'));
    assert_eq!(s.selected, 1, "j moves down in command focus");
    press(&mut s, KeyCode::Char('k'));
    assert_eq!(s.selected, 0, "k moves up");
    press(&mut s, KeyCode::Char('?'));
    assert!(s.help_open);
    press(&mut s, KeyCode::Char('x'));
    assert!(!s.help_open, "any key dismisses help");

    // Esc walks outward one step per press, and only quits from the start screen.
    assert_eq!(press(&mut s, KeyCode::Esc), Action::None);
    assert_eq!(s.focus, Focus::Input, "Esc leaves command focus first");
    assert_eq!(press(&mut s, KeyCode::Esc), Action::None);
    assert_eq!(s.stage, Stage::Start, "Esc then clears the query to start");
    assert!(s.query.is_empty());
    assert_eq!(
        press(&mut s, KeyCode::Esc),
        Action::Quit,
        "Esc from the start screen quits"
    );

    // Ctrl-C quits from either focus; `q` only quits in command focus.
    let mut s = state();
    assert_eq!(ctrl(&mut s, 'c'), Action::Quit);
    for c in "git".chars() {
        press(&mut s, KeyCode::Char(c));
    }
    assert_eq!(press(&mut s, KeyCode::Char('q')), Action::None);
    assert_eq!(s.query, "gitq", "`q` in input focus is just a character");
    press(&mut s, KeyCode::Backspace);
    press(&mut s, KeyCode::Enter);
    assert_eq!(press(&mut s, KeyCode::Char('q')), Action::Quit);

    // Ctrl-Y copies from any focus, and copies the public doc URL.
    let mut s = state();
    for c in "read".chars() {
        press(&mut s, KeyCode::Char(c));
    }
    let copied = ctrl(&mut s, 'y');
    assert_eq!(
        copied,
        Action::Copy("https://codewandler.github.io/flux/docs/language/ops#read".into()),
        "Ctrl-Y copies the selected op's public doc link from input focus"
    );

    // Bracketed paste appends wholesale rather than arriving as keystrokes.
    let mut s = state();
    s.on_paste("  web_fetch  ");
    assert_eq!(s.query, "web_fetch", "paste is trimmed and appended");
    assert_eq!(s.stage, Stage::Results);
}

/// The start screen draws the constellation above a centered input, and says what it is.
#[test]
fn start_state_renders_pictogram_and_centered_input() {
    let mut s = state();
    let screen = draw(&mut s, 80, 24);
    assert!(
        screen.contains('◆'),
        "the pictogram's nodes are on the start screen:\n{screen}"
    );
    assert!(
        screen.contains("type to search 6 operations"),
        "the hint names the catalog size:\n{screen}"
    );

    // "Centered" is a claim worth checking rather than eyeballing: the node rows must sit
    // symmetrically about the middle column.
    let picto_line = screen
        .lines()
        .find(|l| l.contains('◆'))
        .expect("a pictogram row");
    let left = picto_line.len() - picto_line.trim_start().len();
    let right = picto_line.len() - picto_line.trim_end().len();
    assert!(
        left.abs_diff(right) <= 2,
        "pictogram is not centered (left {left}, right {right}): {picto_line:?}"
    );
}

/// The results split lists on the left and details on the right, and the detail pane carries the
/// facts the story names.
#[test]
fn results_layout_lists_and_details() {
    let mut s = state();
    for c in "write".chars() {
        press(&mut s, KeyCode::Char(c));
    }
    // Tall enough for the whole detail pane: this test is about *what* the panes contain, and the
    // degradation ladder for short/narrow terminals is `min_sizes_degrade_without_panic`'s job.
    let screen = draw(&mut s, 100, 30);

    assert!(
        screen.contains("write"),
        "the matching op is listed:\n{screen}"
    );
    assert!(screen.contains('▸'), "the selection glyph is drawn");
    assert!(
        screen.contains("Do write things."),
        "the list shows the first sentence only:\n{screen}"
    );
    assert!(
        !screen.contains("must not show"),
        "the list must not show the second sentence:\n{screen}"
    );
    for fact in ["risk", "idempotency", "effects", "group", "source"] {
        assert!(
            screen.contains(fact),
            "detail pane is missing `{fact}`:\n{screen}"
        );
    }
    assert!(
        screen.contains("parameters") && screen.contains("required"),
        "params render with their required marker:\n{screen}"
    );
    assert!(
        screen.contains("codewandler.github.io") && screen.contains("127.0.0.1:8788"),
        "both doc URLs are shown:\n{screen}"
    );
    assert!(
        screen.contains("needs `flux docs`"),
        "the local URL is labelled as needing the docs server:\n{screen}"
    );

    // Required params sort ahead of optional ones.
    let params_at = screen.find("parameters").expect("params header");
    let path_at = screen[params_at..].find("path").expect("required param");
    let limit_at = screen[params_at..].find("limit").expect("optional param");
    assert!(path_at < limit_at, "required params come first:\n{screen}");
}

/// Every size from the split threshold down to 1×1 renders without panicking, and the narrow
/// layouts degrade in the specified order: split → single pane → floor message.
#[test]
fn min_sizes_degrade_without_panic() {
    for (w, h) in [
        (200, 60),
        (100, 30),
        (SPLIT_MIN_COLS, 20),
        (SPLIT_MIN_COLS - 1, 20),
        (40, 12),
        (24, 6),
        (20, 5),
        (10, 3),
        (1, 1),
    ] {
        let mut s = state();
        let _ = draw(&mut s, w, h); // start screen
        for c in "git".chars() {
            press(&mut s, KeyCode::Char(c));
        }
        let _ = draw(&mut s, w, h); // results
        press(&mut s, KeyCode::Enter);
        press(&mut s, KeyCode::Char('?'));
        let _ = draw(&mut s, w, h); // help overlay, which sizes itself off the area
    }

    // The floor states its case rather than drawing a smear.
    let mut s = state();
    assert!(draw(&mut s, 20, 5).contains("terminal too"));

    // Just under the split threshold, the detail pane is gone but the list survives.
    let mut s = state();
    for c in "write".chars() {
        press(&mut s, KeyCode::Char(c));
    }
    let narrow = draw(&mut s, SPLIT_MIN_COLS - 1, 20);
    assert!(
        narrow.contains("write"),
        "the list survives a narrow terminal:\n{narrow}"
    );
    assert!(
        !narrow.contains("idempotency"),
        "the detail pane is dropped below the split threshold:\n{narrow}"
    );
}

/// Under a colorless theme the start screen must be static and uncolored — no animation frames, no
/// styling. This is the accessibility contract, and it is also what makes the surface diffable.
#[test]
fn mono_theme_start_state_is_static_and_uncolored() {
    let mut s = ExplorerState::new(
        fixture(),
        OpsExplorerOptions {
            theme: Theme::MONO,
            seed: 0xC643,
        },
    );
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();

    terminal.draw(|f| render(f, &mut s)).unwrap();
    let first = terminal.backend().buffer().clone();
    for _ in 0..12 {
        terminal.draw(|f| render(f, &mut s)).unwrap();
    }
    let later = terminal.backend().buffer().clone();
    assert_eq!(
        first, later,
        "the MONO start screen animated: repeated draws must be identical"
    );

    // Nothing carries a foreground color, and the shape is still there.
    let colored = first
        .content()
        .iter()
        .filter(|c| c.fg != ratatui::style::Color::Reset)
        .count();
    assert_eq!(
        colored, 0,
        "the MONO start screen painted {colored} colored cells"
    );
    let rendered: String = first.content().iter().map(|c| c.symbol()).collect();
    assert!(
        rendered.contains('◆'),
        "the constellation still renders in MONO"
    );
}
