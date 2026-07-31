//! What it takes to arm a golden *rewrite* — the regression test for C-326.
//!
//! `skill_in_sync` and `website_in_sync` write their goldens when armed. The bug this pins shut was
//! that arming tested **presence, not value**: `std::env::var("UPDATE").is_ok()` meant `UPDATE=0`,
//! and even an empty `UPDATE=`, rewrote every golden and let the guard report `ok` having compared
//! nothing. So the assertions below are about *values*, and about the fact that the old, ambient,
//! un-prefixed name is not consulted at all any more.
//!
//! This lives in its own test target because it mutates the process environment: doing that inside
//! `skill_in_sync`/`website_in_sync` would race the guards running beside it on other threads.

#[path = "support/golden_mode.rs"]
mod golden_mode;

use golden_mode::{mode_from, Mode};

#[test]
fn an_unset_or_empty_variable_is_a_check_not_a_rewrite() {
    assert_eq!(
        mode_from(None),
        Ok(Mode::Check),
        "unset must only ever check"
    );
    assert_eq!(
        mode_from(Some("")),
        Ok(Mode::Check),
        "an empty value is not an opt-in — it is what a shell exports by accident"
    );
}

#[test]
fn only_the_exact_armed_value_rewrites() {
    assert_eq!(mode_from(Some("1")), Ok(Mode::Rewrite));
    for not_an_opt_in in ["0", "false", "no", "yes", "true", "2", " 1", "1 "] {
        let refused = mode_from(Some(not_an_opt_in))
            .expect_err(&format!("{not_an_opt_in:?} must not arm a rewrite"));
        assert!(
            refused.contains(golden_mode::VAR) && refused.contains(not_an_opt_in),
            "the refusal names the variable and what it saw: {refused}"
        );
    }
}

/// The heart of C-326: a machine with an ambient `UPDATE` exported must still *check*.
#[test]
fn the_old_ambient_update_variable_no_longer_arms_anything() {
    for ambient in ["0", "", "1"] {
        std::env::set_var("UPDATE", ambient);
        std::env::remove_var(golden_mode::VAR);
        assert_eq!(
            golden_mode::mode(),
            Mode::Check,
            "UPDATE={ambient:?} must not arm a rewrite — only {} does",
            golden_mode::VAR
        );
    }
    std::env::remove_var("UPDATE");

    std::env::set_var(golden_mode::VAR, golden_mode::ARMED);
    assert_eq!(golden_mode::mode(), Mode::Rewrite);
    std::env::remove_var(golden_mode::VAR);
}

/// A rewrite must be impossible to mistake for a verified run: `rewrote` fails rather than
/// returning, so libtest can never print `ok` for a run that only wrote files.
#[test]
fn reporting_a_rewrite_fails_the_run_and_names_the_file() {
    let panicked = std::panic::catch_unwind(|| {
        golden_mode::rewrote(std::path::Path::new("some/golden.md"));
    })
    .expect_err("a rewrite must not be able to return successfully");
    let message = panicked
        .downcast_ref::<String>()
        .map(String::as_str)
        .unwrap_or_default();
    assert!(
        message.contains("REGENERATED") && message.contains("some/golden.md"),
        "the failure says what was written: {message}"
    );
    assert!(
        message.contains("verified nothing"),
        "the failure says the run verified nothing: {message}"
    );
}
