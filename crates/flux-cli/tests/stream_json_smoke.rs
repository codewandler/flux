//! C-160: black-box gate for `flux run --stream-json` / `--stream-json-input` — the real binary,
//! an isolated HOME + CWD, and the offline `-m mock` provider (same harness shape as
//! `mock_smoke.rs`).
//!
//! Spawns set `FLUX_SANDBOX=off`: C-262 makes auto-approved non-interactive surfaces fail closed
//! without an OS sandbox backend, which no stock CI runner has. Confinement posture is asserted in
//! `sandbox_posture.rs`, not here — do not remove it, or this file only passes where `bwrap` exists.

use std::io::Write as _;
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

/// Parse every non-blank stdout line as JSON, panicking with full context on the first line that
/// isn't — the acceptance's "parseable by `jq` with no filtering".
fn parse_ndjson_lines(stdout: &str, stderr: &str) -> Vec<serde_json::Value> {
    stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            serde_json::from_str::<serde_json::Value>(l).unwrap_or_else(|e| {
                panic!("stdout line is not valid JSON ({e}): {l:?}\nfull stdout:\n{stdout}\nstderr:\n{stderr}")
            })
        })
        .collect()
}

#[test]
fn stream_json_emits_the_expected_ndjson_line_sequence_for_a_mock_run() {
    let tmp = TempDir::new("stream-json-mock");
    let work = tmp.path();
    let home = work.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_flux"))
        .args([
            "run",
            "--stream-json",
            "--yes",
            "-m",
            "mock",
            "write a quick note",
        ])
        .current_dir(work)
        .env("HOME", &home)
        .env("NO_COLOR", "1")
        .env("FLUX_SANDBOX", "off")
        .stdin(Stdio::null())
        .output()
        .expect("spawn flux");

    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "`flux run --stream-json -m mock` exited non-zero\nstdout: {stdout}\nstderr: {stderr}"
    );

    let lines = parse_ndjson_lines(&stdout, &stderr);
    assert!(
        !lines.is_empty(),
        "no NDJSON lines on stdout\nstderr: {stderr}"
    );

    // Every line carries a type discriminator and a schema version (acceptance: "each line
    // carrying a `type` discriminator and a schema version").
    for line in &lines {
        assert!(
            line.get("type").and_then(|t| t.as_str()).is_some(),
            "line missing `type`: {line}"
        );
        assert!(
            line.get("v").and_then(|v| v.as_u64()).is_some(),
            "line missing `v`: {line}"
        );
    }

    let types: Vec<&str> = lines.iter().map(|l| l["type"].as_str().unwrap()).collect();
    assert_eq!(
        types.first().copied(),
        Some("turn_start"),
        "first line must be turn_start: {types:?}"
    );
    assert_eq!(
        types.last().copied(),
        Some("turn_end"),
        "last line must be turn_end: {types:?}"
    );
    assert!(types.contains(&"plan"), "missing plan: {types:?}");
    assert!(types.contains(&"approval"), "missing approval: {types:?}");
    assert!(types.contains(&"tool_call"), "missing tool_call: {types:?}");
    assert!(
        types.contains(&"tool_result"),
        "missing tool_result: {types:?}"
    );

    // C-531: every tool line carries the dispatch id, and each `tool_result` repeats the id of its
    // own `tool_call` — the pairing a client needs once concurrent same-name calls can interleave.
    let dispatch_of = |kind: &str| -> Vec<u64> {
        lines
            .iter()
            .filter(|l| l["type"] == kind)
            .map(|l| {
                l["dispatch"]
                    .as_u64()
                    .unwrap_or_else(|| panic!("{kind} line carries no dispatch id: {l}"))
            })
            .collect()
    };
    let call_ids = dispatch_of("tool_call");
    let result_ids = dispatch_of("tool_result");
    assert!(!call_ids.is_empty());
    for id in &result_ids {
        assert!(
            call_ids.contains(id),
            "a tool_result names a dispatch no tool_call announced: {result_ids:?} vs {call_ids:?}"
        );
    }
    let mut unique = call_ids.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(
        unique.len(),
        call_ids.len(),
        "each dispatch id must be unique: {call_ids:?}"
    );

    // Human-readable rendering is suppressed on stdout: no live-markdown/plan-tree/rule furniture,
    // only JSON lines.
    assert!(
        !stdout.contains('✓') && !stdout.contains('─'),
        "stdout contains human-rendered furniture, not pure NDJSON:\n{stdout}"
    );

    // The turn_start line names the session + model; the turn_end line carries the final answer.
    let turn_start = &lines[0];
    assert_eq!(turn_start["model"].as_str(), Some("mock"));
    assert!(turn_start["session"]
        .as_str()
        .is_some_and(|s| !s.is_empty()));
    let turn_end = lines.last().unwrap();
    assert_eq!(turn_end["answer"].as_str(), Some("Finished."));
    assert_eq!(turn_end["outcome"].as_str(), Some("ok"));
    assert!(turn_end.get("error").is_none());

    // The write actually ran (same guarded execution as the human-rendered path).
    let file = work.join("flux-mock.txt");
    assert!(
        file.exists(),
        "flux-mock.txt was not written\nstdout: {stdout}\nstderr: {stderr}"
    );
}

#[test]
fn provider_stage_failure_emits_a_typed_terminal_error_and_exits_nonzero() {
    let tmp = TempDir::new("stream-json-provider-error");
    let work = tmp.path();
    let home = work.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_flux"))
        .args([
            "--no-sandbox",
            "run",
            "--stream-json",
            "--yes",
            "-m",
            "mock",
            "say hi",
        ])
        .current_dir(work)
        .env("HOME", &home)
        .env("NO_COLOR", "1")
        .env("FLUX_SANDBOX", "off")
        .env("FLUX_MOCK_ERROR", "deterministic provider outage")
        .stdin(Stdio::null())
        .output()
        .expect("spawn flux");

    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        !out.status.success(),
        "provider failure must make `flux run` nonzero\nstdout: {stdout}\nstderr: {stderr}"
    );

    let lines = parse_ndjson_lines(&stdout, &stderr);
    let error = lines
        .iter()
        .find(|line| line["type"] == "error")
        .expect("dedicated error line");
    let turn_end = lines.last().expect("terminal turn_end line");
    assert_eq!(turn_end["type"], "turn_end");
    assert_eq!(turn_end["outcome"], "error");

    let message = error["message"].as_str().expect("error message");
    let terminal_error = turn_end["error"].as_str().expect("turn_end error");
    assert_eq!(message, terminal_error, "terminal signals must agree");
    assert!(message.contains("Intent detection failed"), "{message}");
    assert!(
        message.contains("deterministic provider outage"),
        "{message}"
    );
    assert!(
        turn_end["answer"]
            .as_str()
            .is_some_and(|answer| !answer.trim().is_empty()),
        "the human-facing answer must survive: {turn_end}"
    );
}

#[test]
fn stream_json_input_drives_two_sequential_turns_in_one_process() {
    let tmp = TempDir::new("stream-json-input-multi");
    let work = tmp.path();
    let home = work.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_flux"))
        .args(["run", "--stream-json-input", "--yes", "-m", "mock"])
        .current_dir(work)
        .env("HOME", &home)
        .env("NO_COLOR", "1")
        .env("FLUX_SANDBOX", "off")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn flux");

    {
        let stdin = child.stdin.as_mut().expect("stdin");
        writeln!(stdin, r#"{{"text": "first message"}}"#).unwrap();
        writeln!(stdin, r#"{{"text": "second message"}}"#).unwrap();
        // Dropping `child`'s stdin handle at scope end closes it (EOF), ending the conversation
        // after the second turn.
    }

    let out = child.wait_with_output().expect("wait for flux");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "`flux run --stream-json-input` exited non-zero\nstdout: {stdout}\nstderr: {stderr}"
    );

    let lines = parse_ndjson_lines(&stdout, &stderr);
    let turn_starts: Vec<&serde_json::Value> =
        lines.iter().filter(|l| l["type"] == "turn_start").collect();
    let turn_ends: Vec<&serde_json::Value> =
        lines.iter().filter(|l| l["type"] == "turn_end").collect();
    assert_eq!(
        turn_starts.len(),
        2,
        "expected 2 turn_start lines (one per stdin message): {lines:#?}"
    );
    assert_eq!(turn_ends.len(), 2, "expected 2 turn_end lines: {lines:#?}");
    assert_eq!(turn_starts[0]["input"].as_str(), Some("first message"));
    assert_eq!(turn_starts[1]["input"].as_str(), Some("second message"));
}

/// The other half of the "Test covers both" acceptance item: a `steer: true` line that arrives with
/// no turn running has nothing to steer, so it falls back to becoming the next ordinary turn's
/// input (see the design doc's "Input framing") — it must not be silently dropped or leave the
/// process stuck waiting.
#[test]
fn a_steer_line_with_no_turn_running_becomes_an_ordinary_turn() {
    let tmp = TempDir::new("stream-json-input-idle-steer");
    let work = tmp.path();
    let home = work.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_flux"))
        .args(["run", "--stream-json-input", "--yes", "-m", "mock"])
        .current_dir(work)
        .env("HOME", &home)
        .env("NO_COLOR", "1")
        .env("FLUX_SANDBOX", "off")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn flux");

    {
        let stdin = child.stdin.as_mut().expect("stdin");
        // No turn is running yet when this line is parsed — the idle-fallback path.
        writeln!(
            stdin,
            r#"{{"text": "steer with nothing running", "steer": true}}"#
        )
        .unwrap();
    }

    let out = child.wait_with_output().expect("wait for flux");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "`flux run --stream-json-input` exited non-zero\nstdout: {stdout}\nstderr: {stderr}"
    );

    let lines = parse_ndjson_lines(&stdout, &stderr);
    let turn_starts: Vec<&serde_json::Value> =
        lines.iter().filter(|l| l["type"] == "turn_start").collect();
    assert_eq!(
        turn_starts.len(),
        1,
        "the idle steer line must become exactly one ordinary turn: {lines:#?}"
    );
    assert_eq!(
        turn_starts[0]["input"].as_str(),
        Some("steer with nothing running")
    );
    assert!(
        !lines.iter().any(|l| l["type"] == "steered"),
        "nothing was actually steered (no turn was running to steer): {lines:#?}"
    );
}

#[test]
fn stream_json_input_without_yes_is_a_clear_startup_error() {
    let tmp = TempDir::new("stream-json-input-no-yes");
    let work = tmp.path();
    let home = work.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_flux"))
        .args(["run", "--stream-json-input", "-m", "mock", "hello"])
        .current_dir(work)
        .env("HOME", &home)
        .env("NO_COLOR", "1")
        .env("FLUX_SANDBOX", "off")
        .stdin(Stdio::null())
        .output()
        .expect("spawn flux");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "`--stream-json-input` without `--yes` must fail closed"
    );
    assert!(stderr.contains("--yes"), "{stderr}");
}

/// C-160 redaction acceptance item: a `Redactor`-registered secret (here, `FLUX_SECRET` — one of
/// the env vars `seed_provider_env_secrets` seeds from the same way `build_agent_with` does) must
/// never appear in an emitted line, even when it rides in a tool call's *input* (the gap
/// `Executor::dispatch`'s own redaction never covers — see docs/designs/ndjson-agent-protocol.md).
#[test]
fn stream_json_redacts_a_registered_secret_out_of_a_tool_calls_input() {
    let tmp = TempDir::new("stream-json-redaction");
    let work = tmp.path();
    let home = work.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let secret = "sk-mock-super-secret-0123456789";

    let out = Command::new(env!("CARGO_BIN_EXE_flux"))
        .args(["run", "--stream-json", "--yes", "-m", "mock", "write it"])
        .current_dir(work)
        .env("HOME", &home)
        .env("NO_COLOR", "1")
        .env("FLUX_SANDBOX", "off")
        .env("FLUX_SECRET", secret)
        .env("FLUX_MOCK_TOOL", "write")
        .env(
            "FLUX_MOCK_TOOL_INPUT",
            serde_json::json!({"path": "note.txt", "content": secret}).to_string(),
        )
        .stdin(Stdio::null())
        .output()
        .expect("spawn flux");

    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "run failed\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !stdout.contains(secret),
        "the registered secret leaked into stdout:\n{stdout}"
    );
    let lines = parse_ndjson_lines(&stdout, &stderr);
    assert!(
        lines.iter().any(|l| l["type"] == "tool_call"),
        "expected a tool_call line to exercise the input-redaction path: {lines:#?}"
    );
}
