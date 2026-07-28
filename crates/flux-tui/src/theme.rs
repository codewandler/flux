//! The TUI color theme: named semantic roles as ratatui [`Color`]s, plus the ANSI palette the
//! plan-tree renderer paints with (its output is converted to ratatui `Text` via `ansi-to-tui`).
//!
//! Themes are values passed into rendering, so call sites never branch: `dark` (ANSI) with a
//! truecolor variant, `light` for light terminals, and `mono` for `NO_COLOR` — selected via
//! [`Theme::by_name`] and the `/theme` command (C-104). Roles mirror `flux-cli`'s `style.rs` so
//! the CLI and TUI read as the same product.

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
    /// Quiet surface behind the composer. This is its only visual boundary.
    pub composer_bg: Color,
    /// Background for transient sheets (approval/help/session/queue).
    pub panel_bg: Color,
    /// Foreground for chrome text on composer/panel surfaces.
    pub text: Color,
    /// Whole-screen background fill. `Reset` keeps the terminal's own background (dark themes);
    /// light themes must paint it or they look broken on dark terminals.
    pub base_bg: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self::DARK
    }
}

impl Theme {
    /// The default dark theme (ANSI — respects the terminal's own palette).
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
        composer_bg: Color::Indexed(236),
        panel_bg: Color::Indexed(235),
        text: Color::Gray,
        base_bg: Color::Reset,
    };

    /// Truecolor tuning of [`Theme::DARK`] (One-Dark-adjacent, accent matches the splash cyan).
    pub const DARK_RGB: Theme = Theme {
        user: Color::Rgb(229, 192, 123),
        assistant: Color::Reset,
        tool: Color::Rgb(86, 182, 194),
        ok: Color::Rgb(152, 195, 121),
        err: Color::Rgb(224, 108, 117),
        warn: Color::Rgb(229, 192, 123),
        muted: Color::Rgb(102, 110, 124),
        accent: Color::Rgb(52, 211, 200),
        sel_bg: Color::Rgb(58, 63, 74),
        composer_bg: Color::Rgb(44, 48, 56),
        panel_bg: Color::Rgb(38, 42, 50),
        text: Color::Rgb(171, 178, 191),
        base_bg: Color::Reset,
    };

    /// Light theme (ANSI). Paints the whole screen (`base_bg`) so it also works on terminals
    /// whose own background is dark.
    pub const LIGHT: Theme = Theme {
        user: Color::Blue,
        assistant: Color::Black,
        tool: Color::Blue,
        ok: Color::Green,
        err: Color::Red,
        warn: Color::Magenta,
        muted: Color::Indexed(242),
        accent: Color::Blue,
        sel_bg: Color::Indexed(252),
        composer_bg: Color::Indexed(254),
        panel_bg: Color::Indexed(253),
        text: Color::Black,
        base_bg: Color::Indexed(255),
    };

    /// Truecolor tuning of [`Theme::LIGHT`].
    pub const LIGHT_RGB: Theme = Theme {
        user: Color::Rgb(150, 100, 0),
        assistant: Color::Rgb(40, 44, 52),
        tool: Color::Rgb(2, 119, 137),
        ok: Color::Rgb(64, 133, 37),
        err: Color::Rgb(190, 49, 60),
        warn: Color::Rgb(150, 100, 0),
        muted: Color::Rgb(130, 135, 142),
        accent: Color::Rgb(1, 132, 150),
        sel_bg: Color::Rgb(214, 222, 230),
        composer_bg: Color::Rgb(236, 236, 232),
        panel_bg: Color::Rgb(230, 230, 226),
        text: Color::Rgb(40, 44, 52),
        base_bg: Color::Rgb(250, 250, 247),
    };

    /// `NO_COLOR` theme: no colors at all — structure comes from modifiers and glyphs only.
    pub const MONO: Theme = Theme {
        user: Color::Reset,
        assistant: Color::Reset,
        tool: Color::Reset,
        ok: Color::Reset,
        err: Color::Reset,
        warn: Color::Reset,
        muted: Color::Reset,
        accent: Color::Reset,
        sel_bg: Color::Reset,
        composer_bg: Color::Reset,
        panel_bg: Color::Reset,
        text: Color::Reset,
        base_bg: Color::Reset,
    };

    /// Selectable theme names (what `/theme` lists and [`Theme::by_name`] accepts).
    pub fn names() -> &'static [&'static str] {
        &["dark", "light", "mono"]
    }

    /// Resolve a theme name for this terminal: `NO_COLOR` forces [`Theme::MONO`]; truecolor
    /// terminals get the `_RGB` tuning. Unknown names return `None`.
    pub fn by_name(name: &str, truecolor: bool, no_color: bool) -> Option<Theme> {
        if no_color {
            return match name {
                "dark" | "light" | "mono" => Some(Self::MONO),
                _ => None,
            };
        }
        match name {
            "dark" => Some(if truecolor {
                Self::DARK_RGB
            } else {
                Self::DARK
            }),
            "light" => Some(if truecolor {
                Self::LIGHT_RGB
            } else {
                Self::LIGHT
            }),
            "mono" => Some(Self::MONO),
            _ => None,
        }
    }

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
    pub fn composer_style(&self) -> Style {
        Style::default().fg(self.text).bg(self.composer_bg)
    }
    pub fn panel_style(&self) -> Style {
        Style::default().fg(self.text).bg(self.panel_bg)
    }
    pub fn base_style(&self) -> Style {
        Style::default().bg(self.base_bg)
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
