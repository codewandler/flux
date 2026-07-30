//! C-296: `flux eval` names flux-bench — on a channel that cannot corrupt a parsed stream.
//!
//! The supported harness benchmark moved to [flux-bench]; `flux eval` keeps working unchanged and
//! gains a pointer, not a deprecation. What these tests pin is the **routing decision**, the same
//! one `sandbox_posture.rs` pins for the resolved-posture disclosure: a courtesy line belongs on
//! stderr, because stdout carries the scored summary a caller reads and `--report` writes a
//! Markdown artifact a caller diffs. A friendly notice landing in either is a regression, not a
//! nicety.
//!
//! The suite is driven with a task filter that matches nothing, on purpose. Zero tasks still walks
//! the whole output path — the summary to stdout, the rendered report to the `--report` file — while
//! spawning no child agent, so the test is offline, deterministic, and independent of whether an OS
//! sandbox backend exists on the machine running it. What it therefore does **not** cover is
//! per-case output; the assertions are made against the entire stdout and the entire report, so a
//! pointer emitted inside the case loop would still be caught.
//!
//! [flux-bench]: https://github.com/codewandler/flux-bench

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The substring every pointer surface must carry. Deliberately the repository name rather than a
/// full sentence: the wording is free to improve, the destination is not.
const MARK: &str = "flux-bench";

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

/// Run `flux <args>` in an isolated HOME + CWD with every provider credential removed — `flux eval`
/// constructs no provider for a suite that runs no task, so it must succeed with none available.
fn run_flux(dir: &Path, args: &[&str]) -> (String, String, bool) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_flux"));
    cmd.args(args)
        .current_dir(dir)
        .env("HOME", dir)
        // Declare the posture rather than inheriting the host's (C-262): `flux eval` is an
        // auto-approved surface, so a machine with a sandbox backend and a CI runner without one
        // would otherwise take different startup paths through a test whose subject is neither.
        // `off` is honest here — the zero-task filter means no case, and therefore no child
        // process, is ever spawned.
        .env("FLUX_SANDBOX", "off")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("OPENROUTER_API_KEY")
        .stdin(Stdio::null());
    let out = cmd.output().expect("run flux");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.success(),
    )
}

#[test]
fn the_bench_pointer_goes_to_stderr_and_never_into_stdout_or_the_report() {
    // The tag must not itself contain [`MARK`]: `flux eval` echoes the `--report` path on stdout,
    // and a fixture directory named after the destination would fake the assertion below green.
    let tmp = TempDir::new("bptr");
    let report = tmp.path().join("report.md");
    let report_arg = report.to_string_lossy().to_string();
    let (stdout, stderr, ok) = run_flux(
        tmp.path(),
        &[
            "eval",
            "mock",
            "--tasks",
            "__no_such_task__",
            "--report",
            &report_arg,
        ],
    );

    assert!(ok, "`flux eval` must still succeed\nstderr:\n{stderr}");
    assert!(
        stderr.contains(MARK),
        "`flux eval` should point at flux-bench on stderr, got:\n{stderr}"
    );
    assert!(
        !stdout.contains(MARK),
        "the pointer must not reach stdout — a caller parses the summary there:\n{stdout}"
    );

    let written = std::fs::read_to_string(&report).expect("--report file written");
    assert!(
        !written.contains(MARK),
        "the pointer must not reach the `--report` artifact:\n{written}"
    );
}

#[test]
fn eval_help_names_where_the_supported_harness_benchmark_lives() {
    let tmp = TempDir::new("bptr-help");
    let (stdout, stderr, ok) = run_flux(tmp.path(), &["eval", "--help"]);
    assert!(ok, "`flux eval --help` must succeed\nstderr:\n{stderr}");
    assert!(
        stdout.contains(MARK),
        "`flux eval --help` should say where the supported benchmark lives, got:\n{stdout}"
    );
}
