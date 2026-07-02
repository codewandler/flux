//! Native ratatui rendering on the shared [`super::layout`] engine (used by flux-tui's
//! transcript). Output is pre-wrapped with hanging indents baked in — render it WITHOUT a wrapping
//! `Paragraph` (or keep wrap only as a safety net; never `trim`, it would eat the indents).

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};

use super::layout::{layout, Seg};
use crate::parser::parse;

/// A ratatui theme: the `Style` for each rendered role (an empty `Style` disables a role).
#[derive(Debug, Clone)]
pub struct Theme {
    pub heading: Style,
    pub code: Style,
    pub link: Style,
    pub muted: Style,
    pub bold: Style,
    pub italic: Style,
    pub strike: Style,
}

impl Default for Theme {
    /// A dark-terminal default matching [`super::Theme`](super::term::Theme)'s ANSI codes.
    fn default() -> Self {
        Theme {
            heading: Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            code: Style::new().fg(Color::Indexed(180)),
            link: Style::new()
                .fg(Color::Blue)
                .add_modifier(Modifier::UNDERLINED),
            muted: Style::new().add_modifier(Modifier::DIM),
            bold: Style::new().add_modifier(Modifier::BOLD),
            italic: Style::new().add_modifier(Modifier::ITALIC),
            strike: Style::new().add_modifier(Modifier::CROSSED_OUT),
        }
    }
}

impl Theme {
    /// A theme that applies no styling.
    pub fn no_color() -> Self {
        let s = Style::new();
        Theme {
            heading: s,
            code: s,
            link: s,
            muted: s,
            bold: s,
            italic: s,
            strike: s,
        }
    }
}

/// Render `src` as markdown wrapped to `width` columns, styled natively to ratatui spans with the
/// default theme.
pub fn render(src: &str, width: u16) -> Text<'static> {
    render_with_theme(src, &Theme::default(), width as usize)
}

/// Render with an explicit theme and wrap width.
pub fn render_with_theme(src: &str, theme: &Theme, width: usize) -> Text<'static> {
    let doc = parse(src);
    let lines: Vec<Line<'static>> = layout(&doc, width)
        .into_iter()
        .map(|line| Line::from(line.into_iter().map(|s| span(s, theme)).collect::<Vec<_>>()))
        .collect();
    Text::from(lines)
}

fn span(seg: Seg, theme: &Theme) -> Span<'static> {
    let f = seg.style;
    let mut st = if f.heading {
        theme.heading
    } else {
        Style::default()
    };
    if f.bold {
        st = st.patch(theme.bold);
    }
    if f.italic {
        st = st.patch(theme.italic);
    }
    if f.strike {
        st = st.patch(theme.strike);
    }
    if f.code {
        st = st.patch(theme.code);
    }
    if f.link {
        st = st.patch(theme.link);
    }
    if f.muted {
        st = st.patch(theme.muted);
    }
    Span::styled(seg.text, st)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(text: &Text<'_>) -> String {
        text.lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect()
    }

    #[test]
    fn heading_and_list_render_to_multiple_lines() {
        let md = "# Title\n\nsome **bold** prose\n\n- one\n- two\n";
        let text = render(md, 40);
        let s = flat(&text);
        assert!(s.contains("Title"), "heading text present: {s:?}");
        assert!(s.contains("one") && s.contains("two"), "list items present");
        assert!(
            text.lines.iter().any(|l| l.spans.len() > 1),
            "styled spans produced"
        );
    }

    #[test]
    fn plain_text_survives() {
        let text = render("just a sentence", 40);
        assert!(flat(&text).contains("just a sentence"));
    }

    #[test]
    fn heading_span_carries_style() {
        let text = render("# Title\n", 40);
        let span = text.lines[0]
            .spans
            .iter()
            .find(|s| s.content.contains("Title"))
            .expect("title span");
        assert!(span.style.add_modifier.contains(Modifier::BOLD));
    }

    /// The deliberate fix over the replaced stack: a nested list keeps its parent's marker on the
    /// parent's own line and indents children under it.
    #[test]
    fn nested_list_renders_correctly() {
        let text = render("- parent\n  - child one\n  - child two\n- second\n", 60);
        let lines: Vec<String> = text
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(
            lines,
            vec![
                "• parent".to_string(),
                "  • child one".to_string(),
                "  • child two".to_string(),
                "• second".to_string(),
            ]
        );
    }
}
