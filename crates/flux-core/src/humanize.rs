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

/// A compact "… ago" string from two millisecond epoch timestamps (C-151): `5s ago` / `3m ago` /
/// `2h ago` / `4d ago`. `now_ms` is a caller-supplied parameter rather than read from the wall
/// clock in here, so every call site — and every test of one — is deterministic. Shared by every
/// surface that lists sessions (`flux sessions`, the TUI session picker) so the tiers stay in one
/// place instead of drifting the way `fmt_count` once did.
pub fn fmt_age(now_ms: i64, then_ms: i64) -> String {
    let secs = ((now_ms - then_ms) / 1000).max(0);
    match secs {
        s if s < 60 => format!("{s}s ago"),
        s if s < 3_600 => format!("{}m ago", s / 60),
        s if s < 86_400 => format!("{}h ago", s / 3_600),
        s => format!("{}d ago", s / 86_400),
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

    #[test]
    fn fmt_age_scales() {
        // C-151: fully deterministic — fixed `now_ms`/`then_ms` inputs, no wall clock involved.
        assert_eq!(fmt_age(10_000, 5_000), "5s ago");
        assert_eq!(fmt_age(200_000, 5_000), "3m ago");
        assert_eq!(fmt_age(3_600_000 * 2 + 5_000, 5_000), "2h ago");
        assert_eq!(fmt_age(86_400_000 * 4 + 5_000, 5_000), "4d ago");
        // A `then_ms` at or after `now_ms` (clock skew, or "just created") never goes negative.
        assert_eq!(fmt_age(5_000, 5_000), "0s ago");
        assert_eq!(fmt_age(5_000, 9_000), "0s ago");
    }
}
