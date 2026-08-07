//! C-686: the `flux tui --attach` command-line surface, asserted against the real binary.
//!
//! Two things are load-bearing here and neither is testable from inside the crate (`flux-cli` has
//! no library target):
//!
//! 1. **`--attach` cannot be combined with `--remote`/`--host`.** Those select an execution
//!    *substrate* for an agent that still runs here, so you still approve here; `--attach` moves
//!    the whole agent, approval stage included. Spelling them together is not a configuration with
//!    a sensible meaning — it is an operator who has confused the two, and the cheapest place to
//!    catch that is argument parsing, before anything connects.
//! 2. **The credential is named, never given.** There is no `--attach-token`; the flag carries the
//!    *name of an environment variable*, so a bearer token for a production agent cannot end up in
//!    a shell history, a process listing or a CI log.

use std::process::Command;

/// Run `flux <args>` and return `(success, stdout, stderr)`.
fn flux(args: &[&str]) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_flux"))
        .env("FLUX_SANDBOX", "off")
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("run `flux {}`: {e}", args.join(" ")));
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

fn tui_help() -> String {
    let (ok, stdout, stderr) = flux(&["tui", "--help"]);
    assert!(ok, "`flux tui --help` failed: {stderr}");
    stdout
}

#[test]
fn attaching_to_a_served_agent_is_a_documented_option_on_flux_tui() {
    let help = tui_help();
    assert!(
        help.contains("--attach"),
        "`flux tui` must offer the attach selection: {help}"
    );
    assert!(
        help.contains("--attach-token-env"),
        "…and name where its credential lives: {help}"
    );
    assert!(
        help.contains("--attach-context"),
        "…and let an operator continue an existing remote conversation: {help}"
    );
}

/// The naming must not be confusable with the substrate axis, so the help says which is which.
#[test]
fn the_help_distinguishes_attaching_an_agent_from_selecting_a_substrate() {
    let help = tui_help();
    let attach_section = help
        .split("--attach ")
        .nth(1)
        .expect("the --attach entry is in the help");
    assert!(
        attach_section.contains("--remote"),
        "the attach help must name the flag it is confusable with: {attach_section}"
    );
    assert!(
        attach_section.contains("flux sessions"),
        "…and say that the conversation is not a local session: {attach_section}"
    );
}

#[test]
fn attach_and_the_substrate_selectors_are_refused_together() {
    for other in [
        vec!["--remote", "https://substrate.example:8790"],
        vec!["--host", "build-farm"],
    ] {
        let mut args = vec!["tui", "--attach", "https://agent.example:8787"];
        args.extend(other.iter().copied());
        let (ok, _stdout, stderr) = flux(&args);
        assert!(
            !ok,
            "`flux {}` must be refused: attaching an agent and selecting a substrate for a local \
             agent are opposite postures",
            args.join(" ")
        );
        assert!(
            stderr.contains("cannot be used with"),
            "the refusal must be clap's conflict, not a downstream failure: {stderr}"
        );
    }
}

/// The bearer credential is never a command-line value.
#[test]
fn there_is_no_flag_that_takes_the_token_itself() {
    let help = tui_help();
    assert!(
        !help.contains("--attach-token "),
        "a `--attach-token <VALUE>` flag would put a production bearer token in argv: {help}"
    );
    let (ok, _stdout, stderr) = flux(&[
        "tui",
        "--attach",
        "https://agent.example:8787",
        "--attach-token",
        "s3cr3t",
    ]);
    assert!(!ok, "`--attach-token` must not exist");
    assert!(
        stderr.contains("--attach-token"),
        "the refusal should name the rejected flag: {stderr}"
    );
}

/// A URL carrying `user:pass@` is the other way a credential reaches argv. It is refused before
/// anything connects, and the refusal does not echo the credential back.
#[test]
fn a_url_with_embedded_credentials_is_refused_without_echoing_it() {
    let (ok, _stdout, stderr) = flux(&["tui", "--attach", "https://alice:hunter2@agent.example"]);
    assert!(!ok, "an embedded credential must be refused");
    assert!(
        stderr.contains("credential-free"),
        "the refusal must say what is wrong: {stderr}"
    );
    assert!(
        !stderr.contains("hunter2"),
        "the refusal must not echo the credential: {stderr}"
    );
}

/// `--attach-token-env` and `--attach-context` are meaningless without a target, and clap says so
/// rather than silently ignoring them.
#[test]
fn the_attach_modifiers_require_a_target() {
    for modifier in [
        vec!["--attach-token-env", "SOME_VAR"],
        vec!["--attach-context", "ctx-1"],
    ] {
        let mut args = vec!["tui"];
        args.extend(modifier.iter().copied());
        let (ok, _stdout, stderr) = flux(&args);
        assert!(!ok, "`flux {}` must be refused", args.join(" "));
        assert!(
            stderr.contains("--attach"),
            "the refusal must name the missing selection: {stderr}"
        );
    }
}
