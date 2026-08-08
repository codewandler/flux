//! D-179: the Agent Lab CLI end-to-end — `flux record` writes a fixture, `flux test` replays it
//! offline as a gate, and the fixture is an ordinary `Storage::dir` store the existing Time Machine
//! tools open via `--store`.
//!
//! Runs the real binary against the offline `mock` provider under an isolated HOME + CWD, so nothing
//! here touches the developer's own `~/.flux` or needs a credential.
//!
//! Every spawn sets `FLUX_SANDBOX=off`. C-262 makes auto-approved non-interactive surfaces —
//! `record --yes` and `run --yes` among them — fail closed unless an OS sandbox backend is available,
//! so on a host without `bwrap` (every stock CI runner) the command refuses to start before doing any
//! work. That posture is asserted in `sandbox_posture.rs`, which is also where tests that require the
//! *absence* of a backend live; installing `bwrap` in CI would break those instead. These tests are
//! about behaviour other than confinement, so they declare unconfined operation rather than depending
//! on the runner having a backend. Do not drop it to "tidy up" — without it this file passes only on a
//! developer machine that happens to have bubblewrap installed.

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

/// Run `flux <args>` in `work` under an isolated HOME. Returns (success, stdout, stderr).
fn flux(work: &Path, args: &[&str]) -> (bool, String, String) {
    let home = work.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_flux"))
        .args(args)
        .current_dir(work)
        .env("HOME", &home)
        .env("NO_COLOR", "1")
        .env("FLUX_SANDBOX", "off")
        .stdin(Stdio::null())
        .output()
        .expect("spawn flux");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The headline: record a scenario, then replay it offline as a passing gate — and prove the
/// fixture is a plain `Storage::dir` store the shipped Time Machine commands read through `--store`
/// (the cross-tool compatibility the fixture format exists to guarantee).
#[test]
fn record_writes_a_fixture_that_flux_test_and_the_time_machine_both_read() {
    let tmp = TempDir::new("agent-lab-e2e");
    let work = tmp.path();

    let (ok, stdout, stderr) = flux(
        work,
        &[
            "record",
            "--yes",
            "-m",
            "mock",
            "note-taking",
            "write a quick note",
        ],
    );
    assert!(
        ok,
        "`flux record` failed\nstdout: {stdout}\nstderr: {stderr}"
    );

    let fixture = work.join("tests/scenarios/note-taking");
    for f in [
        "events.db",
        "flow.db",
        "agent-loops",
        "model.jsonl",
        "plan.flux.snap",
        "scenario.toml",
    ] {
        assert!(
            fixture.join(f).exists(),
            "fixture is missing {f}\nstdout: {stdout}\nstderr: {stderr}"
        );
    }

    // The gate: the REAL agent re-runs against the recorded world — no key, no network, $0.
    let (ok, stdout, stderr) = flux(work, &["test", "note-taking"]);
    assert!(
        ok,
        "`flux test` should pass on a freshly recorded fixture\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(stdout.contains("ok note-taking"), "stdout: {stdout}");

    // Same run, machine-readable — what CI reads.
    let (ok, stdout, _) = flux(work, &["test", "--json"]);
    assert!(ok, "`flux test --json` failed: {stdout}");
    let report: serde_json::Value = serde_json::from_str(&stdout).expect("JSON report");
    assert_eq!(report["failed"], 0, "{report}");
    assert_eq!(report["total"], 1, "{report}");

    // Cross-tool compatibility: the fixture IS a session store, so `flux replay --store` opens it
    // with no fixture-specific code path.
    let store = fixture.to_string_lossy().into_owned();
    let (ok, stdout, stderr) = flux(work, &["replay", "--store", &store, "last"]);
    assert!(
        ok,
        "`flux replay --store <fixture>` failed\nstdout: {stdout}\nstderr: {stderr}"
    );

    // ...and so does `flux sessions`, which lists the recorded session.
    let (ok, stdout, stderr) = flux(work, &["sessions", "--store", &store]);
    assert!(ok, "`flux sessions --store <fixture>` failed: {stderr}");
    assert!(
        stdout.contains("s_") || stdout.contains("msg"),
        "the fixture's session should be listed\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// A corrupted golden makes `flux test` fail with a non-zero exit — it is a real gate, not a
/// reporter. The regression output names the mismatch so a human can act on it.
#[test]
fn flux_test_exits_non_zero_when_a_fixture_regresses() {
    let tmp = TempDir::new("agent-lab-regress");
    let work = tmp.path();

    let (ok, stdout, stderr) = flux(
        work,
        &[
            "record",
            "--yes",
            "-m",
            "mock",
            "drifted",
            "write a quick note",
        ],
    );
    assert!(
        ok,
        "`flux record` failed\nstdout: {stdout}\nstderr: {stderr}"
    );

    // Rewrite the committed plan snapshot: the agent now plans something different from the golden.
    let snap = work.join("tests/scenarios/drifted/plan.flux.snap");
    std::fs::write(
        &snap,
        "flow drifted\n  return \"something else entirely\"\n",
    )
    .unwrap();

    let (ok, stdout, stderr) = flux(work, &["test", "drifted"]);
    assert!(
        !ok,
        "a regressed fixture must exit non-zero\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(stdout.contains("FAILED drifted"), "stdout: {stdout}");
    assert!(
        stdout.contains("plan snapshot mismatch"),
        "the failure must name what regressed\nstdout: {stdout}"
    );

    // FLUX_GOLDEN=update re-baselines it, and the gate goes green again.
    let home = work.join("home");
    let out = Command::new(env!("CARGO_BIN_EXE_flux"))
        .args(["test", "drifted"])
        .current_dir(work)
        .env("HOME", &home)
        .env("NO_COLOR", "1")
        .env("FLUX_SANDBOX", "off")
        .env("FLUX_GOLDEN", "update")
        .stdin(Stdio::null())
        .output()
        .expect("spawn flux");
    assert!(
        out.status.success(),
        "FLUX_GOLDEN=update must re-baseline: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let (ok, _, _) = flux(work, &["test", "drifted"]);
    assert!(ok, "the re-baselined fixture must pass");
}

/// `flux test` with no fixtures at all is a usage error, not a vacuous pass — a CI gate that
/// silently succeeds because it found nothing to run is worse than no gate.
#[test]
fn flux_test_refuses_to_pass_vacuously() {
    let tmp = TempDir::new("agent-lab-empty");
    let (ok, stdout, stderr) = flux(tmp.path(), &["test"]);
    assert!(!ok, "stdout: {stdout}\nstderr: {stderr}");
    assert!(stderr.contains("no scenario fixtures"), "stderr: {stderr}");
}

/// `flux record` rejects any name that is not a single plain path segment (D-185): separators AND
/// the `.`/`..` special components. Without the dot check, `flux record .. "x"` would target
/// `tests/scenarios/..` (= `tests/`) — and because no `scenario.toml` exists there, the clobber
/// guard never trips, so fixture files land outside the scenarios root entirely.
#[test]
fn flux_record_rejects_names_that_are_not_a_single_plain_segment() {
    let tmp = TempDir::new("agent-lab-record-name-guard");
    let work = tmp.path();
    for bad in ["..", ".", "a/b", "../escape"] {
        let (ok, stdout, stderr) = flux(work, &["record", "--yes", "-m", "mock", bad, "hi"]);
        assert!(
            !ok,
            "`flux record {bad}` should be rejected\nstdout: {stdout}\nstderr: {stderr}"
        );
        assert!(
            stderr.contains("single plain path segment"),
            "stderr: {stderr}"
        );
    }
    // No fixture must have been written anywhere — not even the scenarios root itself.
    assert!(
        !work.join("tests").exists(),
        "an invalid name must not create any fixture directory"
    );
}

/// `flux test <name>` applies the same single-plain-segment guard as `flux record` (D-185): before
/// this story `discover_fixtures` did a bare `dir.join(name)` with zero validation, so `flux test
/// ../../anywhere/fixture` could replay an arbitrary path outside `--dir` — asymmetric with
/// `run_record`'s guard for no good reason.
#[test]
fn flux_test_rejects_names_that_are_not_a_single_plain_segment() {
    let tmp = TempDir::new("agent-lab-test-name-guard");
    let work = tmp.path();
    for bad in ["..", ".", "a/b", "../escape"] {
        let (ok, stdout, stderr) = flux(work, &["test", bad]);
        assert!(
            !ok,
            "`flux test {bad}` should be rejected\nstdout: {stdout}\nstderr: {stderr}"
        );
        assert!(
            stderr.contains("single plain path segment"),
            "stderr: {stderr}"
        );
    }
}
