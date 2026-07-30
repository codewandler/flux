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
//! - **[`sanitize`] drops what is interpreted and neutralizes what draws.** Escape sequences are
//!   consumed whole and control characters dropped, so a payload is text and only text (the
//!   C-113/C-114 approval-modal lesson, one surface over); the glyph blocks that exist in order to
//!   draw ([`is_reserved`]) are replaced one-for-one by a space.
//! - **The mark and the border come from the [`Theme`], through [`agent_block`] /
//!   [`agent_overlay_header`].** The mark is a glyph *plus a modifier*, never a tint, so it
//!   survives `Theme::MONO` (`theme.rs:120`), where every colour role resolves to `Color::Reset` —
//!   the same reasoning C-149 used for the transcript gutter rail and C-154 for the approval risk
//!   tiers.
//!
//! # What is guaranteed, and what is only made harder
//!
//! Being precise here matters more than sounding strong, because C-163's host-UI prompts are told
//! to inherit this invariant rather than write a parallel one — so an overclaim here propagates.
//!
//! **Guaranteed.** A payload cannot style anything (C-220's type, plus no escape sequence survives
//! [`sanitize`]); it cannot produce a glyph from the drawing blocks, so it cannot render a
//! *pixel-accurate* copy of this surface's own chrome; every cell it does paint lies inside a
//! region whose border ring, mark and title style are drawn by the surface from the theme, after
//! the payload and from data the payload cannot reach; and the approval sheet draws last over its
//! own `Clear`ed rect, so a pane cannot change one cell of it. Each of those is asserted, in every
//! slot, by `panes::tests`.
//!
//! **Not guaranteed: that nothing a payload writes can *resemble* a frame.** ASCII `|`, `-`, `+`
//! and `_` approximate a box and cannot be taken away from text without gutting it. [`is_reserved`]
//! therefore raises the cost and removes the accurate imitation; it is not the thing standing
//! between the user and a phish. **That thing is the mark** — surface-drawn, one per pane,
//! unforgeable because its glyph is itself reserved, and legible under `MONO` because it carries
//! modifiers rather than colour. A user who has learnt ` ◆ agent ` is not misled by an ASCII-art
//! box drawn underneath it.
//!
//! **Out of scope by design.** Glyphs the *surface* generates from a payload's values are not
//! filtered: `plan::render_nodes`' `├─` connectors and the `█░` progress bar are drawn from the
//! theme at widths the surface chose. The one derived case that *is* filtered is Markdown, because
//! its renderer turns payload text into spans the payload chose the shape of; [`sanitize_lines`]
//! runs over its output for that reason.
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

/// Glyphs reserved to the surface: a payload character that lands here is replaced by a space.
///
/// **The rule has to be range-shaped, not an enumeration.** Reserving only the glyphs the sheet
/// actually draws — `┌┐└┘─│` — is defeated in one keystroke by `┏┓┗┛━┃`, then by `╔╗╚╝═║`, then by
/// `╭╮╰╯`. Any list of code points loses to variant selection, so the unit of reservation is the
/// *block*, and the criterion is "the glyphs in it exist in order to draw" rather than "the surface
/// happens to use them today". A block reserved this way also lets the surface's own chrome grow
/// inside it without this list having to be remembered.
///
/// The drawing blocks, and what each one protects:
///
/// - **misc technical** (`U+2300`) — horizontal and vertical *scan lines* (`U+23B8`–`U+23BD`, whose
///   Unicode names are literally box and scan lines) and the bracket-extension pieces
///   (`U+239B`–`U+23AD`) that tile into a frame edge;
/// - **box drawing** (`U+2500`) — every bordered block on this surface (the approval sheet, a pane,
///   a plan card) and the transcript's C-149 turn-boundary rail;
/// - **block elements** (`U+2580`) — the `/usage` and progress bars, and [`CURSOR`], the
///   deny-reason caret;
/// - **geometric shapes** (`U+25A0`) — the agent mark itself, the slash menu's `▸`, the session
///   picker's `●`;
/// - **braille** (`U+2800`) — not exotic: [`SPINNER`] *is* Braille, so this is the block a payload
///   would imitate the running indicator from, and its 8-dot patterns tile densely enough to stand
///   in for `█`/`░`;
/// - **misc symbols and arrows** (`U+2B00`), **arrows** (`U+2190`), **geometric shapes extended**
///   (`U+1F780`) and the geometric run of the pictographs block (`U+1F532`–`U+1F53D`) — the fill
///   and arrow lookalikes (`⬛`, `⬆`, `⇑`, `🟥`, `🔺`);
/// - **legacy computing** (`U+1FB00`) — sextants, half-blocks and the terminal-graphics tiles that
///   exist for exactly this purpose;
/// - **CJK compatibility forms** (`U+FE30`) and the **fullwidth/halfwidth symbol tail**
///   (`U+FFE0`–`U+FFEE`, plus `U+FF5C`) — vertical and overline rule forms (`︱`, `﹉`, `￨`, `｜`).
///
/// Then a short list of named rules and attention marks that live in blocks too broad to reserve
/// whole (`U+2015` and the em-dash rules; `⚠` and its lookalikes). **This part is
/// enumeration-shaped and therefore best-effort**, and it is allowed to be: neutralizing `⚡` buys
/// little when a payload can always write the word "WARNING". What it must not do is let a payload
/// draw a *frame*, which is the part above.
///
/// See the module docs for what this rule does and does not buy — ASCII `| - + _` can always
/// approximate a box, so the load-bearing guarantee is the unforgeable mark, not this alphabet.
pub(crate) fn is_reserved(ch: char) -> bool {
    matches!(ch,
        // Blocks whose glyphs exist in order to draw.
        '\u{2300}'..='\u{23ff}'        // misc technical: scan lines, bracket pieces
        | '\u{2500}'..='\u{257f}'      // box drawing
        | '\u{2580}'..='\u{259f}'      // block elements
        | '\u{25a0}'..='\u{25ff}'      // geometric shapes
        | '\u{2800}'..='\u{28ff}'      // braille — the spinner's own block
        | '\u{2190}'..='\u{21ff}'      // arrows
        | '\u{2b00}'..='\u{2bff}'      // misc symbols and arrows
        | '\u{fe30}'..='\u{fe4f}'      // CJK compatibility forms: vertical/overline rules
        | '\u{ffe0}'..='\u{ffee}'      // halfwidth/fullwidth symbol tail
        | '\u{ff5c}'                   // fullwidth vertical line
        | '\u{1f532}'..='\u{1f53d}'    // pictographs: the geometric run
        | '\u{1f780}'..='\u{1f7ff}'    // geometric shapes extended
        | '\u{1fb00}'..='\u{1fbff}'    // legacy computing: sextants, half-blocks
        // Named rules and attention marks whose own blocks are too broad to reserve whole.
        | '\u{2015}' | '\u{2e3a}' | '\u{2e3b}'          // horizontal bar, two/three-em dash
        | '\u{2660}'..='\u{2667}'                       // card suits: `♦` imitates the mark
        | '⚠' | '⚡' | '‼' | '⁉' | '❗' | '❕' | '❌' | '⛔')
}

/// Characters that occupy no column but change how the columns around them are read — the display
/// spoof that survives every width check because it has no width. Dropped.
///
/// This is **enumerated, not categorical**: `std` exposes no Unicode-category test, so this is the
/// `Cf` format characters plus the zero-width fillers and line/paragraph separators, listed by
/// range. Treat it as defence in depth rather than as a closed set — the reordering spoof that
/// actually matters (`RLO`/`LRO`/`RLE`/`LRE`/`PDF`, `U+202A`–`U+202E`) is covered, and because the
/// surface budgets every row by *display width* a leftover zero-width character cannot shift a
/// column or escape a pane's rect. It would only ever be cosmetic noise inside one.
fn is_invisible(ch: char) -> bool {
    matches!(ch,
        '\u{00ad}'                     // soft hyphen
        | '\u{061c}'                   // arabic letter mark
        | '\u{070f}'                   // syriac abbreviation mark
        | '\u{1160}'                   // hangul jungseong filler
        | '\u{180b}'..='\u{180f}'      // mongolian variation selectors and vowel separator
        | '\u{200b}'..='\u{200f}'      // zero-width space/joiners, LRM/RLM
        | '\u{2028}' | '\u{2029}'      // line and paragraph separators
        | '\u{202a}'..='\u{202e}'      // bidi embedding and override
        | '\u{2060}'..='\u{206f}'      // word joiner, invisible operators, bidi isolates, deprecated
        | '\u{3164}'                   // hangul filler
        | '\u{feff}'                   // BOM / zero-width no-break space
        | '\u{ffa0}'                   // halfwidth hangul filler
        | '\u{fff9}'..='\u{fffb}'      // interlinear annotation
        | '\u{1d173}'..='\u{1d17a}'    // musical formatting
        | '\u{e0000}'..='\u{e007f}') // tags
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

    /// The reserved set is checked against the surface's **actual** chrome, so a payload can never
    /// forge a piece of chrome this surface really draws.
    ///
    /// Every glyph the crate holds in a *named constant* is read from that constant — the mark, the
    /// C-149 transcript rail, the deny-reason caret, the spinner frames, the in-flight badge — so
    /// those entries cannot drift from what is rendered. The remainder are inline literals at their
    /// call sites, cited here, and are the reason this test is only half the cross-check: a range
    /// this list forgets is invisible to it. The other half — an adversarial corpus that is *not*
    /// derived from [`is_reserved`] — lives in
    /// `panes::tests::an_agent_pane_cannot_imitate_the_approval_sheet`, so widening the rule and
    /// widening its test are not the same edit. (*Guards tested against their own assumptions* is a
    /// recurring bug class in this tree, and it is what a mirrored range list reproduces.)
    #[test]
    fn every_glyph_the_surface_draws_chrome_with_is_reserved() {
        let mut from_constants: Vec<&str> = vec![AGENT_MARK, GUTTER, CURSOR, RUNNING_BADGE];
        from_constants.extend(SPINNER);
        let literals = [
            "▸", // rendering.rs: the slash and `@` menus' selection marker
            "●", // rendering.rs: the session picker's current-session mark; panes.rs: a finished
            // worker's mark in the C-224 fleet pane
            "◌", // panes.rs: an idle or stalled worker in the C-224 fleet pane (also RUNNING_BADGE)
            "█", // rendering.rs / panes.rs: the `/usage` and progress bars, filled
            "░", // rendering.rs / panes.rs: the same bars, empty
            "⚠", // rendering.rs: the sheet's destructive disclosure
            "↑", // rendering.rs: the sheet's subject-scroll hint
            "↓", // rendering.rs: the same
            "├─", "└─", "│  ", // plan.rs: the tree connectors and their indent guide
            "┌", "┐", "└", "┘", "─", "│", // ratatui's own `Block::bordered` glyph set
        ];
        // Only the drawing part of each constant: `RUNNING_BADGE` is a glyph plus a word, and a
        // letter is not chrome.
        for glyph in from_constants.into_iter().chain(literals) {
            for ch in glyph
                .chars()
                .filter(|c| !c.is_whitespace() && !c.is_alphanumeric())
            {
                assert!(
                    is_reserved(ch),
                    "{ch:?} (in {glyph:?}) is chrome this surface draws, and a payload can draw it"
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
