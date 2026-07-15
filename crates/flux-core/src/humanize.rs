//! Compact humanizers for counts and durations, shared by every surface (CLI, TUI, …).
//!
//! These live at L0 so surfaces call ONE implementation instead of re-deriving thresholds and
//! drifting — the historical bug was the TUI's private copy rounding *after* choosing the unit, so
//! `999_999` rendered `1000.0k` while the CLI's copy (which rounds first) rendered `1.0M`.

use std::time::Duration;

/// Format a count compactly: `940` / `5.4k` / `1.2M`. Rounds to the display precision **before**
/// picking the unit, so the boundary hands off cleanly: `999_950` reads `1.0M`, never `1000.0k`.
pub fn fmt_count(n: u64) -> String {
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
pub fn fmt_elapsed(d: Duration) -> String {
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

    #[test]
    fn fmt_count_scales() {
        assert_eq!(fmt_count(940), "940");
        assert_eq!(fmt_count(5_400), "5.4k");
        assert_eq!(fmt_count(1_200_000), "1.2M");
    }

    #[test]
    fn fmt_count_hands_off_units_at_the_boundary() {
        // Rounding must happen BEFORE the unit is chosen, or `999_999` reads `1000.0k` (the exact
        // bug the TUI's old private `fmt_count` reintroduced) instead of `1.0M`.
        assert_eq!(fmt_count(999_949), "999.9k");
        assert_eq!(fmt_count(999_999), "1.0M", "never `1000.0k`");
        assert_eq!(fmt_count(1_000), "1.0k");
    }

    #[test]
    fn fmt_elapsed_scales() {
        assert_eq!(fmt_elapsed(Duration::from_millis(12)), "12ms");
        assert_eq!(fmt_elapsed(Duration::from_millis(1400)), "1.4s");
    }
}
