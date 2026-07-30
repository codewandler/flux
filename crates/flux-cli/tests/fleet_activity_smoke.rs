//! C-246: a delegated run is visible on the CLI surface, per role, with a last-activity age.
//!
//! A-79 shipped a correlated, redacted sub-agent activity stream and the CLI's sink dropped every
//! event of it, so a wave of workers — the shape a fleet coordinator runs in — produced a long
//! silence an operator could not distinguish from a hang. This drives the **real binary**, fully
//! offline, over an authored flow that fans two `task` calls out in parallel, and asserts the
//! per-worker lines actually reach stderr. Before the change there are no such lines at all.
//!
//! Everything here is offline: `-m mock`, no network, no credentials (they are explicitly removed
//! from the child environment so a developer's real keys cannot influence the run).
//!
//! Spawns set `FLUX_SANDBOX=off`: C-262 makes auto-approved non-interactive surfaces fail closed
//! without an OS sandbox backend, which no stock CI runner has. Confinement posture is asserted in
//! `sandbox_posture.rs`, not here — do not remove it, or this file only passes where `bwrap` exists.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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

/// A sentinel that stands in for worker credential material. It is deliberately **not** registered
/// with the `Redactor`: the claim under test is that the surface never reads a worker's tool input
/// at all, which is stronger than scrubbing it on the way out.
const SENTINEL: &str = "SENTINEL-CREDENTIAL-9f2a";

fn run_fleet_flow(tag: &str) -> (bool, String) {
    let tmp = TempDir::new(tag);
    let work = tmp.path();
    let home = work.join("home");
    std::fs::create_dir_all(&home).expect("create home");
    std::fs::create_dir_all(work.join(".flux/agents")).expect("create agents dir");

    // Two roles so the surface has to keep two workers apart by identity, not by name collision.
    for role in ["prober", "second-prober"] {
        std::fs::write(
            work.join(format!(".flux/agents/{role}.md")),
            "---\ntools: [read]\n---\nYou read one file and report what it says.\n",
        )
        .expect("write role");
    }
    std::fs::write(
        work.join("fleet.flux"),
        "flow fleet_watch() -> String\n\
         \x20 parallel\n\
         \x20   branch $a\n\
         \x20     $a = task({ role: \"prober\", task: \"survey the tree\" })\n\
         \x20   branch $b\n\
         \x20     $b = task({ role: \"second-prober\", task: \"survey it again\" })\n\
         \x20 return $a\n",
    )
    .expect("write flow");
    // The file the workers are told to read. Its NAME is the sentinel, so the secret rides in the
    // child's tool *input* — exactly the field A-79 documents as the internal half of its contract.
    std::fs::write(
        work.join(format!("{SENTINEL}.txt")),
        "nothing secret in here\n",
    )
    .expect("write probe file");

    let out = Command::new(env!("CARGO_BIN_EXE_flux"))
        .args(["flow", "run", "fleet.flux", "-m", "mock", "--yes"])
        .current_dir(work)
        .env("HOME", &home)
        .env("NO_COLOR", "1")
        .env("FLUX_SANDBOX", "off")
        // Wide enough that the assertions read the whole line rather than its truncation.
        .env("COLUMNS", "400")
        .env("FLUX_CASSETTE", "0")
        .env("FLUX_MOCK_TOOL", "read")
        .env(
            "FLUX_MOCK_TOOL_INPUT",
            serde_json::json!({ "path": format!("{SENTINEL}.txt") }).to_string(),
        )
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("CLAUDE_CODE_OAUTH_TOKEN")
        .env_remove("FLUX_ADD_DIRS")
        .env_remove("FLUX_ALLOW_ALL")
        .stdin(Stdio::null())
        .output()
        .expect("spawn flux");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// Only the fleet lines — the surface region this story owns. The parent's own operation cards may
/// legitimately show the parent's arguments; a *worker's* activity line may not.
fn fleet_lines(stderr: &str) -> Vec<&str> {
    stderr
        .lines()
        .filter(|line| line.contains("⚇ fleet"))
        .collect()
}

#[test]
fn a_delegated_wave_is_visible_per_role_on_the_cli_surface() {
    let (ok, stderr) = run_fleet_flow("fleet-activity");
    assert!(ok, "offline flow run failed:\n{stderr}");
    let lines = fleet_lines(&stderr);
    assert!(
        !lines.is_empty(),
        "C-246: no per-worker activity reached the surface — the CLI sink dropped A-79's \
         `subagent.activity` stream:\n{stderr}"
    );
    let joined = lines.join("\n");
    for role in ["prober", "second-prober"] {
        assert!(
            joined.contains(&format!("{role}#")),
            "worker role `{role}` never appeared on the surface:\n{joined}"
        );
    }
    assert!(
        joined.contains("done"),
        "no worker's terminal outcome reached the surface:\n{joined}"
    );
}

#[test]
fn every_worker_line_carries_the_last_activity_age() {
    // The hung-versus-working signal: no fleet line may omit how long its worker has been quiet.
    // Without it the surface can say a worker exists but not whether it is still moving.
    let (ok, stderr) = run_fleet_flow("fleet-idle");
    assert!(ok, "offline flow run failed:\n{stderr}");
    let lines = fleet_lines(&stderr);
    assert!(!lines.is_empty(), "no fleet lines:\n{stderr}");
    for line in &lines {
        assert!(
            line.contains("idle "),
            "a fleet line without a last-activity age cannot distinguish a hung worker \
             from a working one: {line}"
        );
    }
}

#[test]
fn a_workers_tool_input_never_reaches_the_surface() {
    // The redaction/default-deny claim, end to end through the real binary: the worker really did
    // call `read` with the sentinel in its arguments, and the surface named the operation without
    // ever carrying the argument.
    let (ok, stderr) = run_fleet_flow("fleet-redaction");
    assert!(ok, "offline flow run failed:\n{stderr}");
    let lines = fleet_lines(&stderr);
    assert!(!lines.is_empty(), "no fleet lines:\n{stderr}");
    let joined = lines.join("\n");
    assert!(
        joined.contains("running read"),
        "the worker's operation was never named, so this test would pass vacuously:\n{joined}"
    );
    assert!(
        !joined.contains(SENTINEL),
        "a worker's tool input reached the surface:\n{joined}"
    );
}
