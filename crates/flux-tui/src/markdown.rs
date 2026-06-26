//! Render assistant Markdown to ratatui [`Text`].
//!
//! Pipeline: `markdown-stream` parses the text to a flat event stream and `markdown-ratatui` renders
//! those events straight to styled ratatui spans — natively, with no ANSI round-trip. The whole
//! transcript is one wrapped `Paragraph`, so this pre-wraps to the transcript's inner width (with
//! list hanging indents baked in) to keep line math honest. Only *finalized* assistant turns go
//! through here — a streaming partial renders as plain text + a cursor (half-parsed Markdown
//! flickers), which the caller handles.

use ratatui::text::Text;

/// Render `src` as GFM Markdown wrapped to `width` columns, styled natively to ratatui spans.
pub fn render(src: &str, width: u16) -> Text<'static> {
    let events = markdown_stream::parse_gfm(src);
    markdown_ratatui::render_with(&events, &markdown_ratatui::Theme::default(), width as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_and_list_render_to_multiple_lines() {
        let md = "# Title\n\nsome **bold** prose\n\n- one\n- two\n";
        let text = render(md, 40);
        let flat: String = text
            .lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(flat.contains("Title"), "heading text present: {flat:?}");
        assert!(
            flat.contains("one") && flat.contains("two"),
            "list items present"
        );
        // markdown produces styled spans (bold), not one flat span
        assert!(
            text.lines.iter().any(|l| l.spans.len() > 1),
            "styled spans produced"
        );
    }

    #[test]
    fn plain_text_survives() {
        let text = render("just a sentence", 40);
        let flat: String = text
            .lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(flat.contains("just a sentence"));
    }
}
