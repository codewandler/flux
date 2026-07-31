//! How the pinned-wire guard decides between *checking* and *regenerating* — the one place that
//! reads the arming variable.
//!
//! Two rules, both of them scars (C-326):
//!
//! 1. **Presence is not consent.** The old gate was `std::env::var("UPDATE").is_ok()`, which tests
//!    presence, not value: `UPDATE=0` — or even an empty `UPDATE=` — armed a rewrite. `UPDATE` is a
//!    name an unrelated tool or a shell profile can export, so on such a machine the wire golden was
//!    overwritten with whatever the code currently emitted and the test reported **ok** having
//!    compared nothing — which is precisely the change this crate exists to make you look at. The
//!    variable is now `FLUX_UPDATE_GOLDEN` (crate-scoped, like every other behaviour-changing
//!    variable in this workspace) and only the exact value `1` arms it.
//! 2. **A rewrite may never be reported as a check.** Writing used to `return` before the
//!    `assert_eq!`, so a regenerating run and a verifying run were indistinguishable in libtest's
//!    output — both printed `ok`. A rewriting run now *fails*, naming the file it wrote. See
//!    [`rewrote`].
//!
//! This is a deliberate copy of `crates/flux-lang/tests/support/golden_mode.rs`: this crate ships on
//! the independent plugin-protocol version line (C-143) and must not gain a dependency on flux-lang
//! for a test helper. Keep the two in step; each is covered by its own `golden_arming` test.

#![allow(dead_code)] // the including test target uses a subset

/// The variable that arms regeneration.
pub const VAR: &str = "FLUX_UPDATE_GOLDEN";

/// The only value that arms it.
pub const ARMED: &str = "1";

/// What this run is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Compare the golden against what the code emits, and fail on drift.
    Check,
    /// Overwrite the golden, then fail the run so it cannot pass for a check.
    Rewrite,
}

/// Decide from the raw variable, without touching the environment.
///
/// Unset or empty is a *check* — an ambient or blank value must never arm a rewrite. Exactly
/// [`ARMED`] is a rewrite. Anything else is refused rather than guessed at: quietly checking would
/// hand back a green run the author read as "regenerated", and quietly rewriting would bless
/// whatever the code emits on the strength of a typo.
pub fn mode_from(raw: Option<&str>) -> Result<Mode, String> {
    match raw {
        None => Ok(Mode::Check),
        Some("") => Ok(Mode::Check),
        Some(v) if v == ARMED => Ok(Mode::Rewrite),
        Some(other) => Err(format!(
            "{VAR}={other} is not a value this guard recognizes.\n\
             Set `{VAR}={ARMED}` to regenerate the goldens, or leave it unset to check them.\n\
             Refusing to guess: checking would look like a verified run and rewriting would bless \
             whatever the code currently emits."
        )),
    }
}

/// [`mode_from`] applied to this process's environment. Panics on an unrecognized value.
pub fn mode() -> Mode {
    let raw = std::env::var(VAR).ok();
    mode_from(raw.as_deref()).unwrap_or_else(|e| panic!("{e}"))
}

/// Report a golden that was just rewritten — by **failing**.
///
/// This is deliberate and is the whole point of the second rule above: a run that wrote a file
/// verified nothing, so it must not be able to print `ok`. libtest catches this panic per test, so
/// every guard in the binary still writes its own golden before the run goes red; the failure is
/// the receipt, not an error.
pub fn rewrote(path: &std::path::Path) -> ! {
    panic!(
        "REGENERATED {} — this run wrote the golden and verified nothing.\n\
         The failure is expected: it is how a regenerating run ({VAR}={ARMED}) is told apart from a \
         checking run. Review the diff, then re-run with {VAR} unset to actually verify.",
        path.display()
    )
}
