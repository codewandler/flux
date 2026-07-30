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

/// Silent when confinement actually holds or was never requested — a warning that fires when
/// nothing is wrong trains operators to ignore it. Covers the shipped default (`FLUX_SANDBOX`
/// unset → `off`) and the nested case (a truthy `FLUX_SANDBOXED`, i.e. `Backend::AlreadyConfined`,
/// where an outer flux sandbox already confines this whole process tree).
#[test]
fn no_disclosure_when_the_sandbox_is_off_or_confinement_is_inherited() {
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
    // Non-vacuity for the nested arm: prove this run really did take the `AlreadyConfined` branch
    // rather than, say, failing before `apply_sandbox_env` ever ran. The pre-existing dim note is
    // that branch's observable marker, so asserting it also pins the branch the disclosure change
    // deliberately left alone.
    assert!(
        nested.stderr.contains("already confined by the outer flux run"),
        "expected the nested run to reach the AlreadyConfined branch.\nstderr:\n{}",
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

/// Once per **process**, not once per **spawn** — the bullet a `wrap_argv`-level warning would
/// violate, burying the signal in exactly the sessions that spawn most. `FLUX_ENABLE_BASH` +
/// `FLUX_MOCK_BASH` drive the offline mock provider into a real `bash` op, so this run genuinely
/// creates an OS subprocess through the same `build_command` choke point the sandbox wraps; the
/// echoed marker is asserted so the test cannot pass by silently never spawning at all.
#[test]
fn the_disclosure_is_once_per_process_even_when_the_run_spawns_a_subprocess() {
    let run = run_flux_with_env(
        "c217-spawn",
        Some("on"),
        false,
        &[
            ("FLUX_ENABLE_BASH", "1"),
            ("FLUX_MOCK_BASH", "echo c217-spawn-marker"),
        ],
        &["run", "--yes", "-m", "mock", "run a command"],
    );

    let both = format!("{}{}", run.stdout, run.stderr);
    assert!(
        both.contains("c217-spawn-marker"),
        "the mock run must actually spawn a subprocess for this test to mean anything.\nstdout:\n{}\nstderr:\n{}",
        run.stdout,
        run.stderr
    );
    assert_eq!(
        run.stderr.matches("UNCONFINED").count(),
        1,
        "a real spawn must not add a second disclosure.\nstderr:\n{}",
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
        &[
            "run",
            "--stream-json",
            "--yes",
            "-m",
            "mock",
            "write a quick note",
        ],
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

/// Acceptance: **once per process, not once per spawn.** The tests above run subcommands that spawn
/// nothing, so they cannot tell the two apart. This one drives a mock turn that actually executes
/// the `bash` op — a real child process built through `System::build_command`, i.e. through
/// `Sandbox::wrap_argv`, the exact seam where a naive implementation would warn. Exactly one
/// disclosure must survive that.
#[test]
fn the_disclosure_is_emitted_once_per_process_not_once_per_spawn() {
    let run = run_flux_with_env(
        "c217-spawn",
        Some("on"),
        false,
        // `bash` is opt-in (off-by-default `shell` group), so it must be enabled explicitly.
        &[("FLUX_ENABLE_BASH", "1"), ("FLUX_MOCK_BASH", "echo hello")],
        &[
            "run",
            "--stream-json",
            "--yes",
            "-m",
            "mock",
            "run a command",
        ],
    );

    // Non-vacuity: assert the spawn really happened. Without this, a run that errored out before
    // executing anything would trivially satisfy the count assertion below.
    assert!(
        run.stdout.contains("\"name\":\"bash\"") && run.stdout.contains("hello"),
        "expected the run to actually spawn a shell process.\nstdout:\n{}\nstderr:\n{}",
        run.stdout,
        run.stderr
    );
    assert_eq!(
        run.stderr.matches("UNCONFINED").count(),
        1,
        "a spawning run must still disclose exactly once.\nstderr:\n{}",
        run.stderr
    );
}
