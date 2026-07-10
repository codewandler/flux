//! Render assistant Markdown to ratatui [`Text`].
//!
//! Delegates to [`flux_markdown::render`], pre-wrapped to the transcript width (including list
//! hanging indents) before the TUI caches the layout and selects its visible viewport. Only
//! *finalized* assistant turns go through here; streaming partials stay plain text so half-parsed
//! Markdown does not flicker.

use ratatui::text::Text;

/// Render `src` as GFM Markdown wrapped to `width` columns, styled natively to ratatui spans.
pub fn render(src: &str, width: u16) -> Text<'static> {
    flux_markdown::render::render(src, width)
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
