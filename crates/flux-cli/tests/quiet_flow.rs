//! `flux flow run -q/--quiet` gate: the stderr progress surface (session line, per-op lines, the
//! turn rule) goes silent, while stdout keeps the flow result and failures stay visible. Drives the
//! real binary under an isolated HOME + CWD like `saved_flows.rs`.
//!
//! Spawns set `FLUX_SANDBOX=off`: C-262 makes auto-approved non-interactive surfaces fail closed
//! without an OS sandbox backend, which no stock CI runner has. Confinement posture is asserted in
//! `sandbox_posture.rs`, not here.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let n = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("flux-quiet-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
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

fn fixture(tag: &str) -> (TempDir, PathBuf, PathBuf) {
    let temp = TempDir::new(tag);
    let work = temp.path().join("work");
    let home = temp.path().join("home");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    (temp, work, home)
}

fn run(work: &Path, home: &Path, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_flux"));
    command
        .args(args)
        .current_dir(work)
        .env("HOME", home)
        .env("NO_COLOR", "1")
        .env("FLUX_SANDBOX", "off")
        .env("FLUX_CASSETTE", "0")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("CLAUDE_CODE_OAUTH_TOKEN")
        .env_remove("AWS_ACCESS_KEY_ID")
        .env_remove("AWS_SECRET_ACCESS_KEY")
        .env_remove("AWS_SESSION_TOKEN")
        .env_remove("FLUX_ADD_DIRS")
        .env_remove("FLUX_ALLOW_ALL")
        .env_remove("FLUX_QUIET")
        .env_remove("FLUX_MOCK_RESPONSE")
        .stdin(Stdio::null());
    for (key, value) in extra_env {
        command.env(key, value);
    }
    command.output().expect("spawn flux")
}

const PURE_FLOW: &str =
    "flow quiet_probe() -> Any\n  $x = merge_obj({objects: [{a: 1}, {b: 2}]})\n  return $x\n";

/// The progress markers `--quiet` must remove: the op dispatch/result lines, the session line, and
/// the turn-end rule with its step count.
fn assert_no_progress(stderr: &str, label: &str) {
    assert!(!stderr.contains("merge_obj"), "{label} stderr: {stderr}");
    assert!(!stderr.contains("flow · "), "{label} stderr: {stderr}");
    assert!(!stderr.contains("step"), "{label} stderr: {stderr}");
}

#[test]
fn quiet_flag_silences_progress_and_keeps_the_result() {
    let (_temp, work, home) = fixture("silence");
    std::fs::write(work.join("pipeline.flux"), PURE_FLOW).unwrap();

    // Baseline: the default surface streams progress — the assertions below must not pass because
    // rendering moved elsewhere.
    let loud = run(
        &work,
        &home,
        &["flow", "run", "pipeline.flux", "--yes"],
        &[],
    );
    assert!(loud.status.success(), "loud run failed: {loud:?}");
    let loud_err = String::from_utf8_lossy(&loud.stderr);
    assert!(loud_err.contains("merge_obj"), "loud stderr: {loud_err}");
    assert!(loud_err.contains("flow · "), "loud stderr: {loud_err}");

    let quiet = run(
        &work,
        &home,
        &["flow", "run", "pipeline.flux", "--yes", "-q"],
        &[],
    );
    assert!(quiet.status.success(), "quiet run failed: {quiet:?}");
    let quiet_out = String::from_utf8_lossy(&quiet.stdout);
    let quiet_err = String::from_utf8_lossy(&quiet.stderr);
    assert!(
        quiet_out.contains("\"a\":1") && quiet_out.contains("\"b\":2"),
        "quiet stdout must keep the flow result: {quiet_out}"
    );
    assert_no_progress(&quiet_err, "quiet");

    // The env spelling behaves exactly like the flag.
    let env_quiet = run(
        &work,
        &home,
        &["flow", "run", "pipeline.flux", "--yes"],
        &[("FLUX_QUIET", "1")],
    );
    assert!(
        env_quiet.status.success(),
        "env quiet run failed: {env_quiet:?}"
    );
    assert_no_progress(&String::from_utf8_lossy(&env_quiet.stderr), "env quiet");
}

#[test]
fn quiet_flag_keeps_failures_visible() {
    let (_temp, work, home) = fixture("failure");
    std::fs::write(
        work.join("pipeline.flux"),
        "flow quiet_probe() -> Any\n  assert false, \"boom probe\"\n  return 1\n",
    )
    .unwrap();

    let quiet = run(
        &work,
        &home,
        &["flow", "run", "pipeline.flux", "--yes", "-q"],
        &[],
    );
    assert!(
        !quiet.status.success(),
        "a failing flow must exit non-zero under --quiet"
    );
    let stderr = String::from_utf8_lossy(&quiet.stderr);
    assert!(
        stderr.contains("boom probe"),
        "the failure must stay visible under --quiet: {stderr}"
    );
}
