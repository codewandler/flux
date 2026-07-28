//! Color/tty-aware styling for the CLI chrome. Every helper **no-ops when color is disabled** (piped
//! stderr, `NO_COLOR`, or `--color never`), so logs and pipes stay free of ANSI escapes.

use std::sync::atomic::{AtomicBool, Ordering};

static COLOR: AtomicBool = AtomicBool::new(false);

/// `--color` choice.
#[derive(Clone, Copy, Debug, Default, clap::ValueEnum)]
pub enum ColorChoice {
    /// Color when BOTH stdout and stderr are terminals and `NO_COLOR` is unset.
    #[default]
    Auto,
    Always,
    Never,
}

/// Decide and store whether color is on (call once in `main`).
///
/// Auto requires BOTH streams to be terminals: the styled output goes to stdout (reports,
/// dashboards) as well as stderr (status lines), and one process-wide toggle can only keep the
/// module's no-ANSI-in-pipes promise by taking the conjunction — `flux usage > report.txt` must
/// not embed escapes in the file just because stderr is still a tty.
pub fn init(choice: ColorChoice) {
    use std::io::IsTerminal;
    let on = match choice {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => {
            std::io::stdout().is_terminal()
                && std::io::stderr().is_terminal()
                && std::env::var_os("NO_COLOR").is_none()
        }
    };
    COLOR.store(on, Ordering::Relaxed);
}

/// Whether ANSI color is on for this run.
pub fn enabled() -> bool {
    COLOR.load(Ordering::Relaxed)
}

/// Whether the terminal advertises 24-bit color (`COLORTERM`), on top of [`enabled`].
/// Gates the truecolor spinner effects; plain-color terminals keep the braille glyph.
pub fn truecolor() -> bool {
    enabled()
        && std::env::var("COLORTERM")
            .map(|v| v.contains("truecolor") || v.contains("24bit"))
            .unwrap_or(false)
}

fn wrap(code: &str, s: &str) -> String {
    if enabled() {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub fn dim(s: &str) -> String {
    wrap("2", s)
}
pub fn bold(s: &str) -> String {
    wrap("1", s)
}
pub fn red(s: &str) -> String {
    wrap("31", s)
}
pub fn green(s: &str) -> String {
    wrap("32", s)
}
pub fn yellow(s: &str) -> String {
    wrap("33", s)
}
pub fn blue(s: &str) -> String {
    wrap("34", s)
}
pub fn cyan(s: &str) -> String {
    wrap("36", s)
}

/// A faint horizontal rule (clamped to a sane width).
pub fn rule(width: usize) -> String {
    dim(&"─".repeat(width.clamp(8, 100)))
}

/// The colored palette for the plan tree — `Palette::PLAIN` when color is off.
pub fn plan_palette() -> flux_flow::render::Palette {
    use flux_flow::render::Palette;
    if enabled() {
        Palette {
            keyword: ("\x1b[35m", "\x1b[0m"),  // magenta
            op: ("\x1b[36m", "\x1b[0m"),       // cyan
            symbol: ("\x1b[1m", "\x1b[0m"),    // bold
            string: ("\x1b[2m", "\x1b[0m"),    // dim
            lit: ("\x1b[2m", "\x1b[0m"),       // dim
            effect: ("\x1b[2m", "\x1b[0m"),    // dim
            connector: ("\x1b[2m", "\x1b[0m"), // dim
            thing: ("\x1b[33m", "\x1b[0m"),    // yellow
        }
    } else {
        Palette::PLAIN
    }
}

// The token/duration humanizers live at L0 (`flux_core::humanize`) so the CLI and TUI share ONE
// implementation. `fmt_tokens` keeps its CLI-local name (a token count) via a rename re-export.
pub use flux_core::humanize::{fmt_count as fmt_tokens, fmt_elapsed};

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    // `COLOR` is a process-global, and cargo runs unit tests concurrently in one process — so the
    // disabled- and enabled-color assertions must live in ONE test. Splitting them lets the two tests
    // race on the global (one flips it to `true` mid-way through the other's `== "x"` checks), which is
    // the source of the historical `helpers_noop_when_disabled` flake. Keep them in a single test.
    #[test]
    fn helpers_track_the_color_toggle() {
        // Disabled: every helper is a no-op passthrough.
        COLOR.store(false, Ordering::Relaxed);
        assert_eq!(dim("x"), "x");
        assert_eq!(cyan("x"), "x");
        assert!(!rule(20).contains('\x1b'));
        assert!(matches!(plan_palette().op, ("", "")));

        // Enabled: helpers wrap with ANSI codes.
        COLOR.store(true, Ordering::Relaxed);
        assert_eq!(green("ok"), "\x1b[32mok\x1b[0m");
        assert!(plan_palette().op.0.contains("36"));

        COLOR.store(false, Ordering::Relaxed); // reset the global for color-reading code elsewhere
    }
}
