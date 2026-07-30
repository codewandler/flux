//! C-217: black-box gate for the `sandbox on` resolved-posture disclosure — the real binary, an
//! isolated HOME + CWD, and both backend-discovery vars forced at nonexistent paths so the backend
//! resolves `Unsupported` deterministically on Linux *and* macOS (the same forcing
//! `apply_sandbox_env_resolves_tightest_wins_and_fails_closed_under_require` relies on).
//!
//! What these tests pin is the **routing decision**: the disclosure goes to stderr, always, and
//! never to stdout. stderr is the channel `--stream-json` already reserves for diagnostics ("the
//! stream is `jq`-parseable with no filtering", `crates/flux-cli/src/args.rs`), so a posture line
//! there cannot corrupt a machine-readable parse — and an operator running under `--stream-json` in
//! production is exactly who must not be left believing they are confined when they are not.

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
