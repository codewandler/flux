//! Render parity (L-02): the new own-AST engine vs the replaced wrapper stack
//! (`markdown-stream` + `markdown-terminal`/`markdown-ratatui`, kept as dev-dependency oracles).
//!
//! Two tiers:
//! 1. **Exact per-line parity** on a snippet suite covering every supported construct, and on the
//!    real committed corpus under `tests/corpus/` — compared at widths 80 and 24, no-color.
//! 2. **Documented divergence**: nested lists. The old stack renders them broken (a parent item's
//!    marker collapses into its first child: `• • parent`); the new engine renders them correctly,
//!    so nested-list snippets are asserted against their *correct* expected output instead
//!    (see `nested_list_fix_over_oracle`). The screened corpus contains no nested lists.

use flux_markdown::render::{render_ansi, Theme};

/// Old terminal renderer (oracle), no color.
fn oracle_terminal(src: &str, width: usize) -> String {
    markdown_terminal::render_with(
        &markdown_stream::parse(src),
        &markdown_terminal::Theme::no_color(),
        width,
    )
}

/// Old ratatui renderer (oracle), flattened to plain per-line strings.
fn oracle_ratatui_lines(src: &str, width: usize) -> Vec<String> {
    let text = markdown_ratatui::render_with(
        &markdown_stream::parse_gfm(src),
        &markdown_ratatui::Theme::no_color(),
        width,
    );
    text.lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect()
}

/// New ratatui renderer, flattened the same way.
fn new_ratatui_lines(src: &str, width: usize) -> Vec<String> {
    let text = flux_markdown::render::render(src, width as u16);
    text.lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect()
}

fn assert_parity(name: &str, src: &str) {
    for width in [80usize, 24] {
        let old = oracle_terminal(src, width);
        let new = render_ansi(src, &Theme::no_color(), width);
        assert_eq!(
            new, old,
            "terminal parity broke for {name} at width {width}\n--- old ---\n{old}\n--- new ---\n{new}"
        );

        let old_lines = oracle_ratatui_lines(src, width);
        let new_lines = new_ratatui_lines(src, width);
        assert_eq!(
            new_lines, old_lines,
            "ratatui parity broke for {name} at width {width}"
        );
    }
}

#[test]
fn snippet_suite_exact_parity() {
    let snippets: &[(&str, &str)] = &[
        ("heading", "# Title\n\n## Sub *styled* heading\n"),
        (
            "paragraph_wrap",
            "A long paragraph that should wrap nicely at both test widths with **bold words** and `inline code` inside it, plus *emphasis* spanning several words.\n",
        ),
        ("tight_list", "- alpha\n- beta gamma delta epsilon zeta eta theta iota kappa\n- gamma\n"),
        ("ordered_list", "1. first\n2. second item that is long enough to wrap at the narrow width\n3. third\n"),
        ("loose_list", "1. first item that is quite long and certainly wraps\n\n2. second item that is also long enough to wrap as well\n"),
        ("blockquote", "> quoted line\n> continues here\n>\n> second quoted paragraph\n"),
        ("fenced_code", "```rust\nfn main() {\n    println!(\"hello\");\n}\n```\n"),
        ("code_no_lang", "```\nplain preformatted text\n```\n"),
        ("thematic_break", "before\n\n---\n\nafter\n"),
        (
            "table",
            "| Name | Count | Note |\n|:-----|------:|------|\n| alpha | 1 | plain |\n| beta | 22 | **bold** cell |\n",
        ),
        ("inline_styles", "plain **strong** *em* ~~strike~~ `code` [link text](https://e.com) <https://auto.link> end\n"),
        ("hard_breaks", "line one  \nline two\\\nline three\n"),
        ("escapes", "\\*literal stars\\* and \\`ticks\\` here\n"),
        ("quote_then_para", "> quote\n\nplain paragraph after\n"),
        ("heading_then_list", "## Heading\n- one\n- two\n"),
        ("mixed_doc", "# Doc\n\nIntro paragraph.\n\n- point one\n- point two\n\n```sh\nls -la\n```\n\nClosing words.\n"),
    ];
    for (name, src) in snippets {
        assert_parity(name, src);
    }
}

/// Exact parity on the real committed corpus (screened: no setext headings, indented code,
/// entities, reference links, task lists, or nested lists; HTML only as comment markers).
///
/// Two files get a documented **visible-line waiver** — every visible line must still match
/// exactly, in order, but blank-line placement (and, where noted, comment-marker lines the oracle
/// leaks) is excluded:
/// - `agents.md`: a blockquote right after a bullet list. The old stack mis-nests it *inside* the
///   list and drops the blank line between the two blocks; the new engine emits the gap.
/// - `lang-skill.md`: `<!-- BEGIN/END generated -->` markers. The old stack hides the BEGIN
///   comment but *prints* the END one (it follows a table without a blank line); the new engine
///   hides both consistently, so comment-marker lines are dropped from the oracle side.
#[test]
fn corpus_exact_parity() {
    let waived = ["agents.md", "lang-skill.md"];
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    let mut checked = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "md") {
            continue;
        }
        let raw = std::fs::read_to_string(&path).unwrap();
        let (_, body) = flux_markdown::split_frontmatter(&raw);
        let name = path.display().to_string();
        if waived
            .iter()
            .any(|w| path.file_name().is_some_and(|f| f == *w))
        {
            // Visible, non-comment lines only (see the waiver rationale above).
            let visible = |s: &str| -> Vec<String> {
                s.lines()
                    .map(str::trim_end)
                    .filter(|l| !l.trim().is_empty())
                    .filter(|l| {
                        let t = l.trim();
                        !(t.starts_with("<!--") || t.ends_with("-->"))
                    })
                    .map(str::to_string)
                    .collect()
            };
            for width in [80usize, 24] {
                let old = visible(&oracle_terminal(body, width));
                let new = visible(&render_ansi(body, &Theme::no_color(), width));
                assert_eq!(
                    new, old,
                    "waiver still requires visible-line parity: {name} at {width}"
                );
                let old_r: Vec<String> = oracle_ratatui_lines(body, width);
                let new_r: Vec<String> = new_ratatui_lines(body, width);
                assert_eq!(
                    visible(&new_r.join("\n")),
                    visible(&old_r.join("\n")),
                    "waiver still requires ratatui visible-line parity: {name} at {width}"
                );
            }
        } else {
            assert_parity(&name, body);
        }
        checked += 1;
    }
    assert!(checked >= 5, "corpus present");
}

/// The one deliberate divergence: the old stack renders nested lists broken (`• • parent`, child
/// misaligned, spurious blank in a tight list). The new engine renders them correctly. This test
/// pins BOTH behaviors so the divergence stays visible and intentional.
#[test]
fn nested_list_fix_over_oracle() {
    let src = "- parent\n  - child one\n  - child two\n- second\n";

    // Old stack (oracle): parent marker collapses into the child line.
    let old = oracle_terminal(src, 80);
    assert!(
        old.starts_with("• • parent"),
        "oracle 'fixed' its nested-list rendering — revisit this waiver: {old:?}"
    );

    // New engine: correct hanging structure.
    let new = render_ansi(src, &Theme::no_color(), 80);
    assert_eq!(new, "• parent\n  • child one\n  • child two\n• second\n");
}

/// Second deliberate divergence: fenced code inside a list item. The old stack emits the code
/// BEFORE the item's own text (its list buffering vs. the renderer's immediate code writes race);
/// the new engine keeps document order and indents the code under the item.
#[test]
fn code_in_list_fix_over_oracle() {
    let src = "- item with code\n  ```\n  fenced inside item\n  ```\n- next item\n";

    let old = oracle_terminal(src, 80);
    assert!(
        old.starts_with("    fenced inside item"),
        "oracle 'fixed' its code-in-list ordering — revisit this waiver: {old:?}"
    );

    let new = render_ansi(src, &Theme::no_color(), 80);
    assert_eq!(
        new,
        "• item with code\n    fenced inside item\n• next item\n"
    );
}
