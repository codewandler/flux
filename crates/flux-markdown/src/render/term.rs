//! ANSI terminal rendering on the shared [`super::layout`] engine, including the streaming
//! [`LiveRenderer`] flux-cli's sinks hold across a model's token stream.

use std::io::{self, Write};

use super::layout::{layout, Seg};
use crate::parser::parse;

/// A terminal theme — raw ANSI SGR sequences per role. An empty string disables a role, so
/// [`Theme::no_color`] emits no escapes at all.
#[derive(Debug, Clone)]
pub struct Theme {
    pub heading: &'static str,
    pub code: &'static str,
    pub link: &'static str,
    pub muted: &'static str,
    pub bold: &'static str,
    pub italic: &'static str,
    pub strike: &'static str,
    pub reset: &'static str,
}

impl Default for Theme {
    /// A sensible dark-terminal default (same codes the replaced wrapper stack used).
    fn default() -> Self {
        Theme {
            heading: "\x1b[1;36m", // bold cyan
            code: "\x1b[38;5;180m",
            link: "\x1b[4;34m", // underline blue
            muted: "\x1b[2m",
            bold: "\x1b[1m",
            italic: "\x1b[3m",
            strike: "\x1b[9m",
            reset: "\x1b[0m",
        }
    }
}

impl Theme {
    /// A theme that emits no escape sequences (non-TTY / `--no-color`).
    pub fn no_color() -> Self {
        Theme {
            heading: "",
            code: "",
            link: "",
            muted: "",
            bold: "",
            italic: "",
            strike: "",
            reset: "",
        }
    }

    /// The styled default when stdout is a terminal and `NO_COLOR` is unset, otherwise
    /// [`Theme::no_color`] (so `… | cat` stays clean).
    pub fn auto() -> Self {
        use std::io::IsTerminal;
        if std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none() {
            Theme::default()
        } else {
            Theme::no_color()
        }
    }
}

/// Render a complete markdown document to an ANSI string, wrapped to `width` columns.
pub fn render_ansi(src: &str, theme: &Theme, width: usize) -> String {
    let doc = parse(src);
    let mut out = String::new();
    for line in layout(&doc, width) {
        for seg in line {
            out.push_str(&styled(&seg, theme));
        }
        out.push('\n');
    }
    out
}

fn styled(seg: &Seg, theme: &Theme) -> String {
    let s = seg.style;
    let mut codes = String::new();
    if s.heading {
        codes.push_str(theme.heading);
        codes.push_str(theme.bold);
    }
    if s.bold {
        codes.push_str(theme.bold);
    }
    if s.italic {
        codes.push_str(theme.italic);
    }
    if s.strike {
        codes.push_str(theme.strike);
    }
    if s.code {
        codes.push_str(theme.code);
    }
    if s.link {
        codes.push_str(theme.link);
    }
    if s.muted {
        codes.push_str(theme.muted);
    }
    if codes.is_empty() {
        seg.text.clone()
    } else {
        format!("{codes}{}{}", seg.text, theme.reset)
    }
}

/// Renders a streaming markdown message, updating the terminal in place: on a TTY each `push`
/// re-renders the accumulated source and redraws over the previous frame (cursor-up + clear);
/// off-TTY it accumulates and renders once at [`LiveRenderer::finish`].
pub struct LiveRenderer {
    theme: Theme,
    width: usize,
    live: bool,
    source: String,
    prev_lines: usize,
}

impl LiveRenderer {
    /// `live` should be true when the output is an interactive terminal (enables the in-place
    /// cursor redraw); false accumulates and renders once at `finish`.
    pub fn new(theme: Theme, width: usize, live: bool) -> Self {
        LiveRenderer {
            theme,
            width,
            live,
            source: String::new(),
            prev_lines: 0,
        }
    }

    /// Whether any text has been pushed since the last `finish`.
    pub fn is_active(&self) -> bool {
        !self.source.is_empty()
    }

    /// Append a chunk of the streaming message and (on a TTY) redraw it in place.
    pub fn push<W: Write>(&mut self, delta: &str, w: &mut W) -> io::Result<()> {
        if delta.is_empty() {
            return Ok(());
        }
        self.source.push_str(delta);
        if self.live {
            self.redraw(w)?;
        }
        Ok(())
    }

    fn redraw<W: Write>(&mut self, w: &mut W) -> io::Result<()> {
        let rendered = render_ansi(&self.source, &self.theme, self.width);
        if self.prev_lines > 0 {
            write!(w, "\x1b[{}A", self.prev_lines)?; // cursor up over the previous render
        }
        write!(w, "\r\x1b[J{rendered}")?; // column 0, clear to end of screen, redraw
        self.prev_lines = rendered.matches('\n').count();
        w.flush()
    }

    /// Commit the current message (it stays on screen) and reset for the next one. When not live,
    /// this is where the accumulated message is rendered (once).
    pub fn finish<W: Write>(&mut self, w: &mut W) -> io::Result<()> {
        if self.source.is_empty() {
            return Ok(());
        }
        if !self.live {
            let rendered = render_ansi(&self.source, &self.theme, self.width);
            write!(w, "{rendered}")?;
            w.flush()?;
        }
        // On a TTY the final state was already drawn by the last `push`; just reset.
        self.source.clear();
        self.prev_lines = 0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_tty_accumulates_and_renders_once_on_finish() {
        let mut live = LiveRenderer::new(Theme::no_color(), 80, false);
        let mut out = Vec::new();
        assert!(!live.is_active());
        live.push("# He", &mut out).unwrap();
        live.push("llo\n\nworld **bold**\n", &mut out).unwrap();
        assert!(live.is_active());
        assert!(out.is_empty(), "nothing rendered until finish off-TTY");
        live.finish(&mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert_eq!(s, "Hello\n\nworld bold\n");
        assert!(!live.is_active(), "finish resets the message");
    }

    #[test]
    fn tty_mode_redraws_in_place() {
        let mut live = LiveRenderer::new(Theme::no_color(), 80, true);
        let mut out = Vec::new();
        live.push("one", &mut out).unwrap();
        live.push(" two", &mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        // First frame draws from a clean line; the second moves up over frame one and redraws.
        assert!(s.starts_with("\r\x1b[Jone\n"), "first frame: {s:?}");
        assert!(s.contains("\x1b[1A\r\x1b[Jone two\n"), "redraw: {s:?}");
        let mut end = Vec::new();
        live.finish(&mut end).unwrap();
        assert!(end.is_empty(), "TTY finish adds nothing (already drawn)");
    }

    #[test]
    fn styled_render_uses_theme_codes() {
        let out = render_ansi("# Title\n", &Theme::default(), 80);
        assert!(
            out.contains("\x1b[1;36m\x1b[1mTitle\x1b[0m"),
            "heading styled: {out:?}"
        );
    }
}
