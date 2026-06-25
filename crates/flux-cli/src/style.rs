//! Color/tty-aware styling for the CLI chrome. Every helper **no-ops when color is disabled** (piped
//! stderr, `NO_COLOR`, or `--color never`), so logs and pipes stay free of ANSI escapes.

use std::sync::atomic::{AtomicBool, Ordering};

static COLOR: AtomicBool = AtomicBool::new(false);

/// `--color` choice.
#[derive(Clone, Copy, Debug, Default, clap::ValueEnum)]
pub enum ColorChoice {
    /// Color when stderr is a terminal and `NO_COLOR` is unset.
    #[default]
    Auto,
    Always,
    Never,
}

/// Decide and store whether color is on (call once in `main`).
pub fn init(choice: ColorChoice) {
    use std::io::IsTerminal;
    let on = match choice {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => {
            std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none()
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

    #[test]
    fn helpers_noop_when_disabled() {
        COLOR.store(false, Ordering::Relaxed);
        assert_eq!(dim("x"), "x");
        assert_eq!(cyan("x"), "x");
        assert!(!rule(20).contains('\x1b'));
        assert!(matches!(plan_palette().op, ("", "")));
    }

    #[test]
    fn helpers_wrap_when_enabled() {
        COLOR.store(true, Ordering::Relaxed);
        assert_eq!(green("ok"), "\x1b[32mok\x1b[0m");
        assert!(plan_palette().op.0.contains("36"));
        COLOR.store(false, Ordering::Relaxed); // reset for other tests
    }

    #[test]
    fn fmt_elapsed_scales() {
        use std::time::Duration;
        assert_eq!(fmt_elapsed(Duration::from_millis(12)), "12ms");
        assert_eq!(fmt_elapsed(Duration::from_millis(1400)), "1.4s");
    }
}
