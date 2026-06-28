//! Markdown rendering — thin wrappers over the `codewandler/markdown` crates.
//!
//! These exist so the rest of flux depends on `flux-markdown` rather than the external crates
//! directly. They are feature-gated (`ratatui`, `terminal`) and add no logic of their own.

/// Render `src` as GFM Markdown wrapped to `width` columns, styled natively to ratatui spans.
///
/// Pipeline: `markdown-stream` parses to a flat event stream and `markdown-ratatui` renders those
/// events straight to styled spans (no ANSI round-trip).
#[cfg(feature = "ratatui")]
pub fn render(src: &str, width: u16) -> ratatui::text::Text<'static> {
    let events = markdown_stream::parse_gfm(src);
    markdown_ratatui::render_with(&events, &markdown_ratatui::Theme::default(), width as usize)
}

/// Live (incremental) markdown rendering to a terminal, re-exported for flux-cli's streaming sink.
#[cfg(feature = "terminal")]
pub use markdown_terminal::{LiveRenderer, Theme};

#[cfg(all(test, feature = "ratatui"))]
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
