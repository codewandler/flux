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

/// Format a token count compactly: `940` / `5.4k` / `1.2M`. Rounds to the display precision
/// BEFORE picking the unit, so the boundary hands off cleanly: `999_950` reads `1.0M`, never
/// `1000.0k`.
pub fn fmt_tokens(n: u64) -> String {
    let tenths_of_k = (n as f64 / 100.0).round();
    if tenths_of_k >= 10_000.0 {
        format!("{:.1}M", tenths_of_k / 10_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", tenths_of_k / 10.0)
    } else {
        n.to_string()
    }
}

/// Format an elapsed duration compactly: `820µs` / `12ms` / `1.4s`.
pub fn fmt_elapsed(d: std::time::Duration) -> String {
    let ms = d.as_millis();
    if ms == 0 {
        format!("{}µs", d.as_micros())
    } else if ms < 1000 {
        format!("{ms}ms")
    } else {
        format!("{:.1}s", d.as_secs_f64())
    }
}

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

    #[test]
    fn fmt_elapsed_scales() {
        use std::time::Duration;
        assert_eq!(fmt_elapsed(Duration::from_millis(12)), "12ms");
        assert_eq!(fmt_elapsed(Duration::from_millis(1400)), "1.4s");
    }

    #[test]
    fn fmt_tokens_scales() {
        assert_eq!(fmt_tokens(940), "940");
        assert_eq!(fmt_tokens(5_400), "5.4k");
        assert_eq!(fmt_tokens(1_200_000), "1.2M");
    }

    #[test]
    fn fmt_tokens_hands_off_units_at_the_boundary() {
        assert_eq!(fmt_tokens(999_949), "999.9k");
        assert_eq!(fmt_tokens(999_999), "1.0M", "never `1000.0k`");
        assert_eq!(fmt_tokens(1_000), "1.0k");
    }
}
