//! The trusted-chrome boundary (C-222): what makes an agent-authored pane structurally unable to
//! pass for harness chrome — the approval sheet above all.
//!
//! The threat is not a rendering bug. It is a pane rendering *correctly* and being read as a
//! trusted prompt: if the model can draw something a human parses as "flux is asking me to approve
//! this", the approval boundary is decorative. [C-163] already wrote the rule for plugins — *"a
//! plugin that can pop a dialog is a plugin that can phish the user inside a trusted surface …
//! constrain the rendering rather than relying on good behavior"* — and it holds harder for the
//! model, which is the thing the approval sheet exists to gate.
//!
//! C-220 closed the *typed* half at the contract end: [`PaneSpec`] has no field that reaches a
//! `Style`, pinned by test. **The reason the model gets no style field is not aesthetic
//! consistency — a style field is the phishing primitive.** This module closes the half a type
//! cannot: content. A payload that may write any glyph can draw its own border, its own risk
//! badge and its own `[y] allow` affordance, and be a counterfeit sheet made entirely of ordinary
//! text.
//!
//! Three things do that, and all three are enforced by construction rather than by a rule the
//! renderer is trusted to remember:
//!
//! - **[`AgentPane`] is the only shape a pane can be rendered from,** its inner [`PaneSpec`] is
//!   private to this module, and its one constructor sanitizes. A payload string therefore cannot
//!   reach a terminal cell without passing through [`sanitize`] — `panes.rs` is a sibling module
//!   and cannot build one any other way.
//! - **[`sanitize`] drops what is interpreted and neutralizes what is chrome.** Escape sequences,
//!   control bytes and invisible/bidi format characters are removed outright (the C-113/C-114
//!   approval-modal lesson, one surface over); the glyph alphabet this surface draws its own
//!   chrome from ([`is_reserved`]) is replaced one-for-one by a space, so a counterfeit frame is
//!   not merely discouraged but unbuildable.
//! - **The mark and the border come from the [`Theme`], through [`agent_block`] /
//!   [`agent_overlay_header`].** The mark is a glyph *plus a modifier*, never a tint, so it
//!   survives `Theme::MONO` (`theme.rs:120`), where every colour role resolves to `Color::Reset` —
//!   the same reasoning C-149 used for the transcript gutter rail and C-154 for the approval risk
//!   tiers.
//!
//! What this module does **not** claim: it does not filter glyphs the *surface* generates from a
//! payload's values — `plan::render_nodes`' `├─` connectors and the `█░` progress bar are drawn by
//! the surface, from the theme, at widths the surface chose. The one derived case that is filtered
//! is Markdown, because its renderer turns payload text into spans the payload chose the shape of;
//! [`sanitize_lines`] runs over its output for that reason.
//!
//! [C-163]: ../../../docs/stories/C-163-plugin-commands-and-host-ui.md

use super::*;

use flux_runtime::{PaneData, PaneNode, PaneSpec};

/// The agent-region mark. One spelling, produced by one function, so the transcript, the side
/// panes and the overlay slot cannot drift into three dialects of the same promise.
pub(crate) const AGENT_MARK: &str = "◆";

/// Columns the mark chip occupies, plus the two separators around a pane title and the two border
/// cells the title sits between. Budgeted off the pane's width before the title is truncated.
const MARK_COLS: usize = 13;

/// The mark as it is drawn: the glyph **and the word**, because a glyph alone asks the user to
/// have learnt it. Built from [`AGENT_MARK`] so there is one glyph, in one place.
fn agent_mark_label() -> String {
    format!(" {AGENT_MARK} agent ")
}

/// The mark span. `REVERSED` inverts foreground and background whatever they are, so the mark is a
/// visible chip under every theme including `MONO`, where `muted` and `panel_bg` are both
/// `Color::Reset`; `BOLD` is the second, independent signal C-149/C-154 rely on.
pub(crate) fn agent_mark_span(theme: &Theme) -> Span<'static> {
    Span::styled(
        agent_mark_label(),
        theme
            .muted_style()
            .add_modifier(Modifier::REVERSED | Modifier::BOLD),
    )
}

/// The title line of an agent-authored region: the surface's mark, then the pane's own title as
/// plain text. `title` is already sanitized (it can only be read off an [`AgentPane`]); it is
/// truncated here to what is left of `width` after the mark.
fn agent_title_line(theme: &Theme, title: &str, width: u16) -> Line<'static> {
    Line::from(vec![
        agent_mark_span(theme),
        Span::styled(
            format!(
                " {} ",
                truncate(title, (width as usize).saturating_sub(MARK_COLS))
            ),
            theme.muted_style(),
        ),
    ])
}

/// The bordered block every side/bottom pane is drawn into. Border style and title style come from
/// the [`Theme`] and from nowhere else — the payload contributes the title's *characters* and
/// nothing about how they are drawn.
pub(crate) fn agent_block(theme: &Theme, title: &str, width: u16) -> Block<'static> {
    Block::bordered()
        .border_style(theme.muted_style())
        .title(agent_title_line(theme, title, width))
}

/// The header row of an `overlay`-slot pane.
///
/// The overlay slot shares [`rendering::render_overlay_panel`]'s chrome with the surface's *own*
/// overlays (help, `/usage`, the queue, the session picker), which is exactly why it needs the
/// mark most: without it an agent pane and a host overlay are the same borderless centred panel.
pub(crate) fn agent_overlay_header(theme: &Theme, title: &str, width: u16) -> Line<'static> {
    let mut line = agent_title_line(theme, title, width);
    // The shared chrome paints its header on `panel_bg`; the mark keeps its own modifiers.
    for span in &mut line.spans {
        span.style = span.style.bg(theme.panel_bg);
    }
    line
}

/// One agent-authored pane, in the only shape the surface can render from.
///
/// The inner [`PaneSpec`] is private to this module and [`AgentPane::sanitized`] is its only
/// constructor, so every string reachable from a rendered pane has been through [`sanitize`]. That
/// is the whole point of the newtype: `panes.rs` is a sibling module, so it cannot assemble one
/// out of a raw spec even by accident.
#[derive(Debug)]
pub(crate) struct AgentPane {
    spec: PaneSpec,
}

impl AgentPane {
    /// Sanitize a spec on its way into the surface's store. `id` is deliberately left alone: it is
    /// an address, never drawn, and rewriting it would break the open/update/close triple.
    pub(crate) fn sanitized(spec: PaneSpec) -> Self {
        Self {
            spec: PaneSpec {
                id: spec.id,
                title: sanitize(&spec.title),
                slot: spec.slot,
                kind: spec.kind,
                lifetime: spec.lifetime,
                data: sanitize_data(spec.data),
            },
        }
    }

    /// The pane's spec. Everything reachable from here is sanitized, because there is no other way
    /// to obtain an [`AgentPane`].
    pub(crate) fn spec(&self) -> &PaneSpec {
        &self.spec
    }

    /// Replace the payload (`pane.update`), re-deriving `kind` from the data so the two can never
    /// disagree — the same invariant `PaneSpec::new` enforces at the contract end — and sanitizing
    /// on the same single path `open` uses, so `update` cannot grow an unfiltered route.
    pub(crate) fn update(&mut self, data: PaneData) {
        self.spec.kind = data.kind();
        self.spec.data = sanitize_data(data);
    }
}

/// U+001B. Matched before [`char::is_control`] so the *sequence* it introduces goes with it.
const ESC: char = '\u{1b}';

/// The glyph alphabet this surface draws its own chrome from. A payload glyph that lands in it is
/// replaced by a space, which is what makes a counterfeit frame unbuildable rather than unlikely:
///
/// - **box drawing** — every bordered block on this surface (the approval sheet, a pane, a plan
///   card) and the transcript's C-149 turn-boundary rail;
/// - **block elements** — the `/usage` and progress bars, and the `▍` deny-reason cursor;
/// - **geometric shapes** — the agent mark itself, the slash menu's `▸`, the session picker's `●`;
/// - `⚠`, `↑`, `↓` — the approval sheet's destructive disclosure and its scroll affordance.
///
/// Ranges rather than an enumeration on purpose: the surface's chrome is allowed to grow inside
/// them without this list having to be remembered.
pub(crate) fn is_reserved(ch: char) -> bool {
    matches!(ch,
        '\u{2500}'..='\u{257F}'      // box drawing
        | '\u{2580}'..='\u{259F}'    // block elements
        | '\u{25A0}'..='\u{25FF}'    // geometric shapes
        | '⚠' | '↑' | '↓')
}

/// Characters that occupy no column but change how the columns around them are read — the display
/// spoof that survives every width check because it has no width. Dropped outright.
fn is_invisible(ch: char) -> bool {
    matches!(ch,
        '\u{00ad}'                   // soft hyphen
        | '\u{200b}'..='\u{200f}'    // zero-width space/joiners, LRM/RLM
        | '\u{202a}'..='\u{202e}'    // bidi embedding and override
        | '\u{2060}'..='\u{2064}'    // word joiner, invisible operators
        | '\u{2066}'..='\u{2069}'    // bidi isolates
        | '\u{feff}') // BOM / zero-width no-break space
}

/// Make one payload string safe to place in a terminal cell: **text, never interpreted**.
///
/// Escape sequences are consumed whole (introducer *and* parameters, so no fragment survives as
/// text), control bytes and invisible/bidi characters are dropped, and [`is_reserved`] glyphs
/// become spaces. The space is one-for-one, not a deletion, so a payload cannot shift its own
/// layout by hiding characters inside chrome either.
///
/// Iteration is over `chars`, so a multi-byte or wide character is never split — the guarded-process
/// invariant's rule (AGENTS.md: truncate untrusted bytes on **char** boundaries, never
/// `String::truncate` at a byte offset) applied wherever untrusted bytes get bounded.
///
/// This is the **single-line** form: a newline is a control character like any other, because
/// every caller of it renders into one row. [`sanitize_block`] is the multi-line form.
pub(crate) fn sanitize(raw: &str) -> String {
    sanitize_inner(raw, false)
}

/// [`sanitize`] for the one payload that is legitimately multi-line: `markdown` source, whose
/// structure *is* its newlines. Everything else is filtered identically — a newline carries no
/// styling and cannot leave the pane's rect, because the surface still owns every row budget.
fn sanitize_block(raw: &str) -> String {
    sanitize_inner(raw, true)
}

fn sanitize_inner(raw: &str, keep_newlines: bool) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            ESC => skip_escape(&mut chars),
            // A tab is cursor motion, not a character; a space is its safe rendering.
            '\t' => out.push(' '),
            '\n' if keep_newlines => out.push('\n'),
            c if c.is_control() || is_invisible(c) => {}
            c if is_reserved(c) => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// Consume the rest of an escape sequence after its `ESC`, per the ECMA-48 grammar, so that no
/// fragment of it survives as text.
fn skip_escape(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    match chars.peek() {
        // CSI: parameter and intermediate bytes, then one final byte in `@`..=`~`.
        Some('[') => {
            chars.next();
            for c in chars.by_ref() {
                if ('\u{40}'..='\u{7e}').contains(&c) {
                    break;
                }
            }
        }
        // OSC / DCS / SOS / PM / APC: a string terminated by BEL or by ST (`ESC \`).
        Some(']' | 'P' | 'X' | '^' | '_') => {
            chars.next();
            while let Some(c) = chars.next() {
                if c == '\u{7}' {
                    break;
                }
                if c == ESC {
                    if chars.peek() == Some(&'\\') {
                        chars.next();
                    }
                    break;
                }
            }
        }
        // Every other escape sequence: zero or more intermediate bytes (`\u{20}`..=`\u{2f}` — the
        // `ESC ( B` charset-designation family), then one final byte.
        _ => {
            for c in chars.by_ref() {
                if !('\u{20}'..='\u{2f}').contains(&c) {
                    break;
                }
            }
        }
    }
}

/// [`sanitize`] over every string in a payload.
fn sanitize_data(data: PaneData) -> PaneData {
    match data {
        PaneData::Rows { header, rows } => PaneData::Rows {
            header: header.iter().map(|c| sanitize(c)).collect(),
            rows: rows
                .iter()
                .map(|row| row.iter().map(|c| sanitize(c)).collect())
                .collect(),
        },
        PaneData::Kv { pairs } => PaneData::Kv {
            pairs: pairs
                .iter()
                .map(|(k, v)| (sanitize(k), sanitize(v)))
                .collect(),
        },
        PaneData::Log { lines } => PaneData::Log {
            lines: lines.iter().map(|l| sanitize(l)).collect(),
        },
        PaneData::Progress { label, done, total } => PaneData::Progress {
            label: sanitize(&label),
            done,
            total,
        },
        PaneData::Tree { roots } => PaneData::Tree {
            roots: sanitize_nodes(&roots, 0),
        },
        PaneData::Markdown { text } => PaneData::Markdown {
            text: sanitize_block(&text),
        },
    }
}

/// [`sanitize`] over a tree's labels, stopping at the depth the renderer stops at.
///
/// Bounding the recursion here matters as much as the sanitizing: nesting is payload-chosen, and
/// `plan::MAX_TREE_DEPTH` is where `plan::render_nodes` and `panes::tree_rows` already stop, so
/// nothing that would have been drawn is lost.
fn sanitize_nodes(nodes: &[PaneNode], depth: usize) -> Vec<PaneNode> {
    if depth >= plan::MAX_TREE_DEPTH {
        return Vec::new();
    }
    nodes
        .iter()
        .map(|node| PaneNode {
            label: sanitize(&node.label),
            children: sanitize_nodes(&node.children, depth + 1),
        })
        .collect()
}

/// [`sanitize`] over already-rendered lines, keeping their styles.
///
/// Used for the one payload path whose glyphs are chosen after the store: Markdown goes through
/// `flux-markdown`, which turns payload text into spans — a thematic break or a table is the
/// renderer emitting chrome the *payload* asked for. Everything else a pane body draws (the tree
/// connectors, the progress bar) is surface-generated from the theme and is left alone.
pub(crate) fn sanitize_lines(lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .map(|mut line| {
            for span in &mut line.spans {
                let clean = sanitize(&span.content);
                if clean != span.content {
                    span.content = clean.into();
                }
            }
            line
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Escape sequences leave nothing behind — not the introducer, not the parameters. A payload
    /// that reaches a terminal cell is text, and only text (the C-113/C-114 approval-modal lesson).
    #[test]
    fn escape_sequences_and_control_bytes_are_removed_whole() {
        assert_eq!(sanitize("\u{1b}[1;31mapproval\u{1b}[0m"), "approval");
        assert_eq!(sanitize("\u{1b}]0;title\u{7}ok"), "ok");
        assert_eq!(sanitize("\u{1b}]8;;http://x\u{1b}\\link"), "link");
        assert_eq!(sanitize("\u{1b}(Bplain"), "plain");
        assert_eq!(sanitize("a\rb\nc\td\u{7f}e\u{9b}f"), "abc def");
        // Only `markdown` keeps its newlines — its structure IS them; nothing else is a row.
        assert_eq!(sanitize_block("# a\n\n- b\n"), "# a\n\n- b\n");
        assert_eq!(sanitize_block("a\rb\u{1b}[2Jc"), "abc");
        // Invisible and bidi-override characters have no width, so no width check catches them.
        assert_eq!(sanitize("y\u{202e}deny\u{202c}"), "ydeny");
        assert_eq!(sanitize("a\u{200b}b\u{feff}c"), "abc");
        // Nothing in a clean string moves.
        assert_eq!(
            sanitize(" approval · destructive "),
            " approval · destructive "
        );
    }

    /// The chrome alphabet is neutralized one glyph for one space, so a counterfeit frame collapses
    /// into blanks instead of shifting the columns around it.
    #[test]
    fn chrome_glyphs_are_replaced_by_spaces_one_for_one() {
        let counterfeit = "┌─ approval · destructive ─┐";
        let clean = sanitize(counterfeit);
        assert_eq!(clean, "   approval · destructive   ");
        assert_eq!(clean.chars().count(), counterfeit.chars().count());
        assert!(!clean.chars().any(is_reserved));
        // The mark itself is reserved: a payload cannot claim to be the surface.
        assert_eq!(sanitize(&format!("{AGENT_MARK} agent")), "  agent");
        // …and neither can it draw the sheet's disclosure or scroll affordances.
        assert_eq!(sanitize("⚠ ↑↓ █░ ▍ ▸ ●"), " ".repeat(13));
    }

    /// The reserved set is checked against the surface's **actual** chrome, not against a copy of
    /// itself: every glyph the harness draws its own chrome from must be in it, or a payload could
    /// forge that piece of chrome. (`guards tested against their own assumptions` is a recurring
    /// bug class in this tree; this is the cross-check that avoids it.)
    #[test]
    fn every_glyph_the_surface_draws_chrome_with_is_reserved() {
        // The mark, the transcript rail (C-149), the bar glyphs the `/usage` and progress panes
        // draw, the deny-reason cursor, the slash-menu and session-picker selection marks, the
        // sheet's destructive disclosure and its scroll hint, and ratatui's own default borders.
        for glyph in [
            AGENT_MARK, GUTTER, "█", "░", "▍", "▸", "●", "⚠", "↑", "↓", "┌", "┐", "└", "┘", "─",
            "│", "├", "┤",
        ] {
            for ch in glyph.chars().filter(|c| !c.is_whitespace()) {
                assert!(
                    is_reserved(ch),
                    "{ch:?} ({glyph:?}) is drawable by a payload"
                );
            }
        }
    }

    /// Sanitizing is char-wise, so a multi-byte or wide payload is never cut mid-character — the
    /// rule AGENTS.md states for the guarded-process invariant, applied where untrusted bytes get
    /// bounded on this surface. The width the surface then budgets is display width, not bytes.
    #[test]
    fn multi_byte_and_wide_payloads_survive_intact_and_truncate_on_char_boundaries() {
        let wide = "日本語のテキスト・émoji🎉・combining é";
        assert_eq!(sanitize(wide), wide);
        for max in 0..UnicodeWidthStr::width(wide) + 4 {
            let cut = truncate(wide, max);
            assert!(
                UnicodeWidthStr::width(cut.as_str()) <= max,
                "{max}: {cut:?} is wider than its budget"
            );
            // Every character kept is a whole character of the source (plus the elision marker).
            assert!(
                cut.chars().all(|c| c == '…' || wide.contains(c)),
                "{max}: {cut:?} contains a fragment"
            );
        }
    }

    /// The Markdown path is the one place a payload chooses glyphs the *renderer* emits, so the
    /// filter runs over its output too.
    #[test]
    fn markdown_output_carries_no_chrome_glyphs() {
        let text = sanitize("# ┌ approval ┐\n\n---\n\n| a | b |\n| - | - |\n| 1 | 2 |\n");
        let lines = sanitize_lines(crate::markdown::render(&text, 40).lines);
        for line in &lines {
            for span in &line.spans {
                assert!(
                    !span.content.chars().any(is_reserved),
                    "markdown emitted chrome: {:?}",
                    span.content
                );
            }
        }
    }

    /// The mark is a glyph **plus a modifier**, never a tint, so it survives `Theme::MONO` where
    /// every colour role is `Color::Reset` — the reasoning C-149 used for the transcript rail and
    /// C-154 for the approval risk tiers.
    #[test]
    fn the_mark_is_a_glyph_and_a_modifier_not_a_tint() {
        for theme in [Theme::MONO, Theme::default(), Theme::LIGHT_RGB] {
            let span = agent_mark_span(&theme);
            assert!(span.content.contains(AGENT_MARK), "the glyph is there");
            assert!(span.content.contains("agent"), "and it is named");
            assert!(
                span.style.add_modifier.contains(Modifier::REVERSED)
                    && span.style.add_modifier.contains(Modifier::BOLD),
                "the mark rides on modifiers, not colour"
            );
        }
    }
}
