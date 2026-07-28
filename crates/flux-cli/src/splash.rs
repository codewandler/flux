//! Decorative FLUX boot splash for the interactive REPL (animation in
//! `flux_tui::splash`). Fully suppressed under `NO_COLOR`/`--color never`,
//! any piped stream, `FLUX_NO_SPLASH`, or a terminal smaller than the box.

use std::io::IsTerminal;

/// Play the splash if the environment allows it; every failure is a silent skip.
pub(super) fn maybe_splash() {
    let tty = std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
        && std::io::stderr().is_terminal();
    let should = should_splash(
        crate::style::enabled(),
        tty,
        flux_system::env_truthy("FLUX_NO_SPLASH"),
        crossterm::terminal::size().ok(),
    );
    if should {
        let _ = flux_tui::splash::play_blocking(flux_tui::splash::Splash::clock_seed());
    }
}

fn should_splash(color: bool, tty: bool, suppressed: bool, size: Option<(u16, u16)>) -> bool {
    color
        && tty
        && !suppressed
        && size.is_some_and(|(cols, rows)| {
            cols >= flux_tui::splash::MIN_COLS && rows >= flux_tui::splash::MIN_ROWS
        })
}

#[cfg(test)]
mod tests {
    use super::should_splash;

    #[test]
    fn splash_requires_color_tty_room_and_no_optout() {
        let big = Some((80, 24));
        assert!(should_splash(true, true, false, big));
        assert!(
            !should_splash(false, true, false, big),
            "NO_COLOR / --color never"
        );
        assert!(!should_splash(true, false, false, big), "piped stream");
        assert!(!should_splash(true, true, true, big), "FLUX_NO_SPLASH");
        assert!(
            !should_splash(true, true, false, Some((63, 24))),
            "too narrow"
        );
        assert!(
            !should_splash(true, true, false, Some((80, 13))),
            "too short"
        );
        assert!(!should_splash(true, true, false, None), "no size probe");
    }
}
