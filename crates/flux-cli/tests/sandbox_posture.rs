//! C-217: black-box gate for the `sandbox on` resolved-posture disclosure — the real binary, an
//! isolated HOME + CWD, and both backend-discovery vars forced at nonexistent paths so the backend
//! resolves `Unsupported` deterministically on Linux *and* macOS (the same forcing
//! `apply_sandbox_env_resolves_tightest_wins_and_fails_closed_under_require` relies on).
//!
//! What these tests pin is the **routing decision**: the disclosure goes to stderr, always, and
//! never to stdout. stderr is the channel `--stream-json` already reserves for diagnostics ("the
//! stream is `jq`-parseable with no filtering", `crates/flux-cli/src/args.rs`), so a posture line
//! there cannot corrupt a machine-readable parse — and an operator running under `--stream-json` in
//! production is exactly who must not be left believing they are confined when they are not. Both
//! machine-readable shapes are covered: the NDJSON stream (`--stream-json`) and the single-document
//! form (`--json`).
//!
//! Note what these tests do NOT claim. Before C-217 this startup surface was already emitting a
//! stderr warning here (`dispatch.rs`, "OS sandbox requested but unavailable …"); it was simply
//! **untested**, so nothing stopped a refactor from dropping it — and the posture fact lived only in
//! a `format!` at L5, invisible to every non-CLI embedder that resolves a `Sandbox`. C-217 moves the
//! fact to L2 (`Sandbox::posture_disclosure`), restates the line truth-first, and pins the whole
//! contract here. See the story's Progress note for the corrected premise.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A temp dir that removes itself on drop — so a failing assertion (a panic) can't leak it.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "flux-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("create temp dir");
        TempDir(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The captured streams of one `flux` invocation.
struct Run {
    stdout: String,
    stderr: String,
    success: bool,
}

/// Run `flux <args>` in an isolated HOME + CWD with no usable sandbox backend. `sandbox` is the
/// `FLUX_SANDBOX` value to request (`None` leaves it unset — the shipped default). `nested` sets the
/// truthy `FLUX_SANDBOXED` marker that means "an outer flux sandbox already confines this tree".
fn run_flux(tag: &str, sandbox: Option<&str>, nested: bool, args: &[&str]) -> Run {
    run_flux_with_env(tag, sandbox, nested, &[], args)
}

/// [`run_flux`] plus `extra` environment entries, for the cases that need a real subprocess spawn
/// (`FLUX_ENABLE_BASH` + `FLUX_MOCK_BASH`) to prove the disclosure is not per-spawn.
fn run_flux_with_env(
    tag: &str,
    sandbox: Option<&str>,
    nested: bool,
    extra: &[(&str, &str)],
    args: &[&str],
) -> Run {
    let tmp = TempDir::new(tag);
    let work = tmp.path();
    let home = work.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_flux"));
    cmd.args(args)
        .current_dir(work)
        .env("HOME", &home)
        .env("NO_COLOR", "1")
        // Force both platforms' discovery vars at nonexistent paths so the backend is `Unsupported`
        // regardless of whether the host actually has bwrap/sandbox-exec installed.
        .env("FLUX_BWRAP_BIN", "/nonexistent/definitely-not-bwrap-c217")
        .env(
            "FLUX_SANDBOX_EXEC_BIN",
            "/nonexistent/definitely-not-sandbox-exec-c217",
        )
        .env_remove("FLUX_SANDBOX")
        .env_remove("FLUX_SANDBOXED")
        .stdin(Stdio::null());
    if let Some(mode) = sandbox {
        cmd.env("FLUX_SANDBOX", mode);
    }
    if nested {
        cmd.env("FLUX_SANDBOXED", "1");
    }
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("spawn flux");
    Run {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        success: out.status.success(),
    }
}

/// Whether `text` carries the resolved-posture disclosure. Keyed on the two load-bearing tokens:
/// the resolved posture (`UNCONFINED`) and the `sandbox:` prefix that identifies the subsystem.
fn discloses_unconfined(text: &str) -> bool {
    text.contains("UNCONFINED") && text.contains("sandbox:")
}

/// The acceptance: `FLUX_SANDBOX=on` with no usable backend states its RESOLVED posture on stderr —
/// unasked-for, naming the reason — instead of degrading silently. `flux sessions` is a cheap,
/// offline, non-agent subcommand: it exercises the startup path (`apply_sandbox_env`) without a
/// provider, and it writes its listing to stdout, which lets the same run prove the routing split.
#[test]
fn sandbox_on_without_a_backend_discloses_the_unconfined_posture_on_stderr() {
    let run = run_flux("c217-on", Some("on"), false, &["sessions"]);

    assert!(
        discloses_unconfined(&run.stderr),
        "`on` + no backend must disclose the resolved posture on stderr.\nstderr:\n{}",
        run.stderr
    );
    // The reason discovery already computed is carried through verbatim, so the operator learns
    // *why* — this is a disclosure story, not a detection story.
    assert!(
        run.stderr.contains("definitely-not-bwrap-c217")
            || run.stderr.contains("definitely-not-sandbox-exec-c217"),
        "the disclosure must name the reason.\nstderr:\n{}",
        run.stderr
    );
    // ROUTING: never on stdout. stdout belongs to the subcommand's own (often machine-read) output.
    assert!(
        !discloses_unconfined(&run.stdout),
        "the disclosure must not reach stdout.\nstdout:\n{}",
        run.stdout
    );
    // Once per process, not once per line of work.
    assert_eq!(
        run.stderr.matches("UNCONFINED").count(),
        1,
        "exactly one disclosure per process.\nstderr:\n{}",
        run.stderr
    );
}

/// Silent when confinement was never requested. A truthy `FLUX_SANDBOXED` still suppresses the
/// unconfined disclosure because it asserts an outer boundary, but accepting that ambient assertion
/// must itself be prominent and auditable.
#[test]
fn no_unconfined_disclosure_when_sandbox_is_off_or_outer_confinement_is_asserted() {
    let default_run = run_flux("c217-default", None, false, &["sessions"]);
    assert!(
        !discloses_unconfined(&default_run.stderr) && !discloses_unconfined(&default_run.stdout),
        "the shipped default never asked to be confined, so it owes no disclosure.\nstderr:\n{}",
        default_run.stderr
    );

    let nested = run_flux("c217-nested", Some("on"), true, &["sessions"]);
    assert!(
        !discloses_unconfined(&nested.stderr) && !discloses_unconfined(&nested.stdout),
        "a nested run IS confined by the outer flux sandbox — no unconfined disclosure is due.\nstderr:\n{}",
        nested.stderr
    );
    // The marker is ambient state and cannot be verified by the nested process. Accepting it must
    // therefore be a prominent, source-attributed trust event rather than a silent escape.
    assert!(
        nested.stderr.contains("FLUX_SANDBOXED=1")
            && nested.stderr.contains("OUTER-CONFINEMENT")
            && nested.stderr.contains("cannot verify"),
        "outer-confinement assertion was not audited prominently.\nstderr:\n{}",
        nested.stderr
    );
}

/// `require` is untouched: it still fails closed at startup rather than disclosing and continuing.
/// The disclosure is additive — it must not have widened what `on` permits, nor softened `require`.
#[test]
fn require_still_fails_closed_instead_of_disclosing_and_continuing() {
    let run = run_flux("c217-require", Some("require"), false, &["sessions"]);
    assert!(
        run.stderr.contains("sandbox required") && run.stderr.contains("unavailable"),
        "`require` + no backend must remain a hard startup error.\nstderr:\n{}",
        run.stderr
    );
    assert!(
        !discloses_unconfined(&run.stderr),
        "`require` never runs unconfined, so it must not claim to.\nstderr:\n{}",
        run.stderr
    );
}

/// The machine-readable contract: under `--stream-json` every stdout line stays valid JSON even
/// though the process is disclosing an unconfined posture. This is *why* stderr was chosen over
/// suppression — the operator still gets told, and `jq` still parses the stream with no filtering.
#[test]
fn the_disclosure_does_not_pollute_stream_json_stdout() {
    let run = run_flux(
        "c217-stream-json",
        Some("on"),
        false,
        &["run", "--stream-json", "-m", "mock", "write a quick note"],
    );

    assert!(
        discloses_unconfined(&run.stderr),
        "the operator must still be told, even in a machine-readable mode.\nstderr:\n{}",
        run.stderr
    );
    let mut lines = 0usize;
    for line in run.stdout.lines().filter(|l| !l.trim().is_empty()) {
        serde_json::from_str::<serde_json::Value>(line).unwrap_or_else(|e| {
            panic!(
                "stdout line is not valid JSON ({e}): {line:?}\nfull stdout:\n{}\nstderr:\n{}",
                run.stdout, run.stderr
            )
        });
        lines += 1;
    }
    // Non-vacuity: an empty stdout would satisfy the loop above without proving anything.
    assert!(
        lines > 0,
        "expected an NDJSON stream on stdout.\nstdout:\n{}\nstderr:\n{}",
        run.stdout,
        run.stderr
    );
}

/// C-262: an auto-approved turn and a serving surface inherit `require` before provider/tool work.
/// With both backends forced unavailable, startup fails rather than silently running unconfined.
#[test]
fn unattended_and_serving_surfaces_fail_closed_before_work() {
    for (tag, args) in [
        (
            "c262-run",
            &["run", "--yes", "-m", "mock", "should never start"][..],
        ),
        (
            "c262-serve",
            &["app", "run", "--serve", "--yes", "-m", "mock"][..],
        ),
        (
            "c262-preset",
            &[
                "preset",
                "map_each",
                "item=item",
                "source=[\"README.md\"]",
                "op=read",
                "collect=results",
                "--run",
                "--yes",
            ][..],
        ),
        (
            "c265-review",
            &["review", "--files", "README.md", "-m", "mock"][..],
        ),
    ] {
        let run = run_flux(tag, None, false, args);
        assert!(!run.success, "{tag} unexpectedly started\n{}", run.stdout);
        assert!(
            run.stderr.contains("unattended sandbox profile refused")
                && run.stderr.contains("container/VM"),
            "{tag} needs an actionable fail-closed error\nstderr:\n{}",
            run.stderr
        );
        assert!(
            !run.stdout.contains("should never start"),
            "provider/tool work reached stdout before preflight: {}",
            run.stdout
        );
    }
}

/// **C-410's failing-first test.** `flux plugin call <name> <op>` invokes a plugin operation
/// headlessly: no interactive approver, and — per this crate's own scoping rule — outside
/// `Executor::dispatch` entirely. It is exactly the shape C-262's profile exists for, and it had no
/// arm in `unattended_sandbox_surface` at all, so it ran at the `Off` default.
///
/// The plugin name is deliberately one that cannot be installed under this test's pinned `$HOME`.
/// That is what makes the assertion sharp: the floor is a *startup* preflight, so the run must die
/// on the posture before it ever resolves a descriptor or spawns a subprocess. Reaching the
/// "not installed" error would mean the profile ran too late to protect the spawn it exists for.
#[test]
fn plugin_call_fails_closed_before_it_reaches_the_plugin() {
    let run = run_flux(
        "c410-plugin-call",
        None,
        false,
        &["plugin", "call", "c410-never-installed", "echo"],
    );
    assert!(
        !run.success,
        "`plugin call` unexpectedly started with no usable sandbox backend\nstdout:\n{}\nstderr:\n{}",
        run.stdout, run.stderr
    );
    assert!(
        run.stderr.contains("unattended sandbox profile refused")
            && run.stderr.contains("container/VM"),
        "`plugin call` needs the same actionable fail-closed error the other headless surfaces \
         get\nstderr:\n{}",
        run.stderr
    );
    assert!(
        !run.stderr.contains("c410-never-installed"),
        "the floor must be applied at startup, before any plugin resolution or spawn\nstderr:\n{}",
        run.stderr
    );
}

/// The other half of the classification, and what keeps the arm above from being a blanket pin on
/// the whole `plugin` subcommand: the operator-facing, non-invoking plugin commands stay on the
/// interactive `off`/`on`/`require` contract. `plugin ls` reads the descriptor store and prints —
/// it invokes no operation, so pinning it would only make plugin *management* impossible on a host
/// with no backend without buying any confinement.
#[test]
fn the_operator_facing_plugin_commands_are_not_pinned_to_the_floor() {
    let run = run_flux("c410-plugin-ls", None, false, &["plugin", "ls"]);
    assert!(
        run.success,
        "`plugin ls` must not inherit the fail-closed floor\nstdout:\n{}\nstderr:\n{}",
        run.stdout, run.stderr
    );
    assert!(
        !run.stderr.contains("unattended sandbox profile refused"),
        "`plugin ls` invokes nothing and must stay on the interactive contract\nstderr:\n{}",
        run.stderr
    );
}

/// `FLUX_SANDBOXED=1` is accepted for real nested runs, but it is also forgeable ambient state.
/// The unattended profile may proceed only with a prominent audit line recording that trust choice.
#[test]
fn unattended_outer_confinement_assertion_is_prominently_audited() {
    let run = run_flux(
        "c262-outer-marker",
        None,
        true,
        &["run", "--yes", "-m", "mock", "hello"],
    );
    assert!(
        run.success,
        "outer-confined nested run should proceed: {}",
        run.stderr
    );
    assert!(
        run.stderr.contains("warning:")
            && run.stderr.contains("FLUX_SANDBOXED=1")
            && run.stderr.contains("OUTER-CONFINEMENT")
            && run.stderr.contains("cannot verify"),
        "outer-confinement trust was not recorded prominently:\n{}",
        run.stderr
    );
    assert!(
        !run.stderr.contains("UNCONFINED"),
        "the assertion is audited, but must not falsely claim the outer boundary is absent:\n{}",
        run.stderr
    );
}

/// The escape is exact and noisy: an operator may deliberately accept an outer isolation boundary,
/// but the process must state the unconfined posture and the source that selected it.
#[test]
fn unattended_unconfined_escape_is_explicit_and_prominent() {
    for (tag, sandbox, args, expected_source) in [
        (
            "c262-escape-flag",
            None,
            &["--no-sandbox", "run", "--yes", "-m", "mock", "hello"][..],
            "--no-sandbox",
        ),
        (
            "c262-escape-env",
            Some("off"),
            &["run", "--yes", "-m", "mock", "hello"][..],
            "FLUX_SANDBOX=off",
        ),
    ] {
        let run = run_flux(tag, sandbox, false, args);
        assert!(run.success, "explicit escape should run: {}", run.stderr);
        assert!(
            run.stderr.contains("profile BYPASSED")
                && run.stderr.contains(expected_source)
                && run.stderr.contains("UNCONFINED"),
            "escape posture was not recorded prominently:\n{}",
            run.stderr
        );
    }
}

/// The other machine-readable surface: a `--json` subcommand emits ONE JSON document on stdout, and
/// the disclosure must not prepend a line to it. `doctor --json` is the sharpest choice — it is
/// offline, and it is also the *on-demand* sandbox surface (`check_sandbox`), so this doubles as a
/// check that the unasked-for disclosure did not disturb the asked-for one.
#[test]
fn the_disclosure_does_not_pollute_json_stdout() {
    let run = run_flux("c217-json", Some("on"), false, &["doctor", "--json"]);

    assert!(
        discloses_unconfined(&run.stderr),
        "the operator must still be told under `--json`.\nstderr:\n{}",
        run.stderr
    );
    let parsed: serde_json::Value = serde_json::from_str(run.stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "`doctor --json` stdout must be one JSON document ({e}).\nstdout:\n{}\nstderr:\n{}",
            run.stdout, run.stderr
        )
    });
    // Non-vacuity: `null` parses fine and would prove nothing about a real report.
    assert!(
        parsed.get("checks").is_some(),
        "expected a real doctor report on stdout.\nstdout:\n{}",
        run.stdout
    );
    assert!(
        !discloses_unconfined(&run.stdout),
        "the disclosure must not reach stdout.\nstdout:\n{}",
        run.stdout
    );
}

/// C-262 compatibility: an auto-approved spawn cannot exercise C-217's soft `on` posture anymore —
/// it correctly upgrades to `require`. Drive the explicit unconfined escape instead and prove its
/// startup audit is one line per process, not one line per spawned child.
#[test]
fn the_unattended_escape_is_recorded_once_per_process_not_once_per_spawn() {
    let run = run_flux_with_env(
        "c217-spawn",
        None,
        false,
        // `bash` is opt-in (off-by-default `shell` group), so it must be enabled explicitly.
        &[("FLUX_ENABLE_BASH", "1"), ("FLUX_MOCK_BASH", "echo hello")],
        &[
            "run",
            "--no-sandbox",
            "--stream-json",
            "--yes",
            "-m",
            "mock",
            "run a command",
        ],
    );

    // Non-vacuity, and note what it deliberately does NOT key on: the marker text `hello` also
    // appears inside the *command string* (`echo hello`), which the plan and `tool_call` events echo
    // back before anything is executed — so searching the output for the marker would pass even if
    // the op were denied and no process ever spawned. Keying on the `tool_result` instead is
    // decisive: that event carries the child's captured stdout, so it exists only if the process
    // really ran.
    assert!(
        run.stdout
            .contains(r#""name":"bash","is_error":false,"content":"hello\n""#),
        "expected a bash tool_result proving a child process actually ran.\nstdout:\n{}\nstderr:\n{}",
        run.stdout,
        run.stderr
    );
    assert_eq!(
        run.stderr.matches("profile BYPASSED").count(),
        1,
        "a spawning escape must be recorded exactly once.\nstderr:\n{}",
        run.stderr
    );
}
