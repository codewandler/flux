//! The TUI color theme: named semantic roles as ratatui [`Color`]s, plus the ANSI palette the
//! plan-tree renderer paints with (its output is converted to ratatui `Text` via `ansi-to-tui`).
//!
//! One dark theme for now; the struct shape (a value passed into rendering) lets us add light/alt
//! themes and a `/theme` command later without touching call sites. Roles mirror `flux-cli`'s
//! `style.rs` so the CLI and TUI read as the same product.

use ratatui::style::{Color, Modifier, Style};

/// A semantic color theme — each field is the color for one role, not a raw escape.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    /// The `›` prompt + user message accent.
    pub user: Color,
    /// Assistant prose (markdown supplies its own emphasis colors; this is the base).
    pub assistant: Color,
    /// Tool verbs (`bash`, `read`, …) and the `→` marker.
    pub tool: Color,
    /// Success (`✓`, badges, ok counts).
    pub ok: Color,
    /// Errors (`✗`, failed badges, error notices).
    pub err: Color,
    /// Warnings (`⚠`, destructive flags).
    pub warn: Color,
    /// De-emphasized chrome (rules, hints, elapsed, result previews).
    pub muted: Color,
    /// Borders / highlights / the active spinner.
    pub accent: Color,
    /// Background for a selected row (slash menu, etc.).
    pub sel_bg: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self::DARK
    }
}

impl Theme {
    /// The default dark theme.
    pub const DARK: Theme = Theme {
        user: Color::Yellow,
        assistant: Color::Reset,
        tool: Color::Cyan,
        ok: Color::Green,
        err: Color::LightRed,
        warn: Color::Yellow,
        muted: Color::DarkGray,
        accent: Color::Cyan,
        sel_bg: Color::Indexed(238),
    };

    pub fn user_style(&self) -> Style {
        Style::default().fg(self.user).add_modifier(Modifier::BOLD)
    }
    pub fn assistant_style(&self) -> Style {
        Style::default().fg(self.assistant)
    }
    pub fn tool_style(&self) -> Style {
        Style::default().fg(self.tool)
    }
    pub fn ok_style(&self) -> Style {
        Style::default().fg(self.ok)
    }
    pub fn err_style(&self) -> Style {
        Style::default().fg(self.err)
    }
    pub fn warn_style(&self) -> Style {
        Style::default().fg(self.warn)
    }
    pub fn muted_style(&self) -> Style {
        Style::default().fg(self.muted)
    }
    pub fn accent_style(&self) -> Style {
        Style::default().fg(self.accent)
    }
}

/// The ANSI `(open, close)` palette the plan tree paints with — mirrors `flux-cli`'s `plan_palette`
/// so the DAG reads identically on both surfaces. `render_styled` wraps each leaf span with these,
/// and `ansi-to-tui` turns the result into styled ratatui `Text`.
pub fn plan_palette() -> flux_flow::render::Palette {
    flux_flow::render::Palette {
        keyword: ("\x1b[35m", "\x1b[0m"),  // magenta
        op: ("\x1b[36m", "\x1b[0m"),       // cyan
        symbol: ("\x1b[1m", "\x1b[0m"),    // bold
        string: ("\x1b[2m", "\x1b[0m"),    // dim
        lit: ("\x1b[2m", "\x1b[0m"),       // dim
        effect: ("\x1b[2m", "\x1b[0m"),    // dim
        connector: ("\x1b[2m", "\x1b[0m"), // dim
        thing: ("\x1b[33m", "\x1b[0m"),    // yellow
    }
}
