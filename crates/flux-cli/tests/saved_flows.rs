//! L-79 gate-level coverage for saved-flow discovery/execution. Every test drives the real binary
//! under an isolated HOME + CWD so provider/session/filesystem behavior cannot be faked by a unit
//! helper.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let n = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("flux-saved-{tag}-{}-{n}", std::process::id()));
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
    std::fs::create_dir_all(work.join(".flux/flows")).unwrap();
    std::fs::create_dir_all(home.join(".flux/flows")).unwrap();
    (temp, work, home)
}

fn run(work: &Path, home: &Path, args: &[&str], mock_response: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_flux"));
    command
        .args(args)
        .current_dir(work)
        .env("HOME", home)
        .env("NO_COLOR", "1")
        .env("FLUX_CASSETTE", "0")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("CLAUDE_CODE_OAUTH_TOKEN")
        .env_remove("AWS_ACCESS_KEY_ID")
        .env_remove("AWS_SECRET_ACCESS_KEY")
        .env_remove("AWS_SESSION_TOKEN")
        .env_remove("FLUX_ADD_DIRS")
        .env_remove("FLUX_ALLOW_ALL")
        .stdin(Stdio::null());
    match mock_response {
        Some(response) => {
            command.env("FLUX_MOCK_RESPONSE", response);
        }
        None => {
            command.env_remove("FLUX_MOCK_RESPONSE");
        }
    }
    command.output().expect("spawn flux")
}

fn assert_success(output: &Output, command: &str) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "{command} failed\nstdout: {stdout}\nstderr: {stderr}"
    );
}

#[test]
fn list_needs_no_agent_session_and_named_deterministic_inputs_need_no_credentials() {
    let (_temp, work, home) = fixture("list-inputs");
    std::fs::write(
        work.join(".flux/flows/deploy.flux"),
        "flow deploy(env: String, replicas: Number)\n  return {env: $env, replicas: $replicas}\n",
    )
    .unwrap();
    std::fs::write(
        home.join(".flux/flows/global-only.flux"),
        "flow global-only\n  return \"global\"\n",
    )
    .unwrap();

    let listed = run(&work, &home, &["flow", "list"], None);
    assert_success(&listed, "flux flow list");
    let stdout = String::from_utf8_lossy(&listed.stdout);
    assert!(
        stdout.contains("deploy [flow]  (params: env, replicas)"),
        "{stdout}"
    );
    assert!(stdout.contains("global-only [flow]"), "{stdout}");
    assert!(
        !home.join(".flux/events.db").exists(),
        "listing must not create an agent session/event store"
    );

    // An anthropic model would need credentials if touched. This deterministic run must remain on
    // the lazy path; JSON is overlaid by repeatable args and the later duplicate wins.
    let executed = run(
        &work,
        &home,
        &[
            "flow",
            "run",
            "deploy",
            "-m",
            "anthropic/claude-sonnet-4-6",
            "--inputs",
            r#"{"env":"dev","replicas":1}"#,
            "--arg",
            "replicas=2",
            "--arg",
            "replicas=3",
        ],
        None,
    );
    assert_success(&executed, "deterministic saved flow");
    assert!(
        String::from_utf8_lossy(&executed.stdout).contains(r#"{"env":"dev","replicas":3}"#),
        "stdout: {}",
        String::from_utf8_lossy(&executed.stdout)
    );
}

#[test]
fn existing_paths_win_and_flow_home_module_ops_are_not_double_installed() {
    let (_temp, work, home) = fixture("path-composite");
    std::fs::write(
        work.join("winner"),
        "flow from-path\n  return \"path-wins\"\n",
    )
    .unwrap();
    std::fs::write(
        work.join(".flux/flows/winner.flux"),
        "flow winner\n  return \"stored-loses\"\n",
    )
    .unwrap();
    let path_run = run(&work, &home, &["flow", "run", "winner"], None);
    assert_success(&path_run, "existing path target");
    assert!(
        String::from_utf8_lossy(&path_run.stdout).contains("path-wins"),
        "stdout: {}",
        String::from_utf8_lossy(&path_run.stdout)
    );

    let mixed = home.join(".flux/flows/mixed.flux");
    std::fs::write(
        &mixed,
        "op decorate(value: String) -> String\n  return fmt(\"{value}!\")\n\nflow mixed\n  $result = decorate(\"ok\")\n  return $result\n",
    )
    .unwrap();
    let path = mixed.to_string_lossy().into_owned();
    let mixed_path = run(&work, &home, &["flow", "run", &path], None);
    assert_success(&mixed_path, "mixed module by path");
    assert!(
        String::from_utf8_lossy(&mixed_path.stdout).contains("ok!"),
        "stdout: {}",
        String::from_utf8_lossy(&mixed_path.stdout)
    );

    // Name resolution relies on the already auto-loaded composite exactly once.
    let mixed_name = run(&work, &home, &["flow", "run", "mixed"], None);
    assert_success(&mixed_name, "mixed module by saved name");
    assert!(
        String::from_utf8_lossy(&mixed_name.stdout).contains("ok!"),
        "stdout: {}",
        String::from_utf8_lossy(&mixed_name.stdout)
    );
}

#[test]
fn stubbed_extract_maps_inputs_and_malformed_output_stops_before_the_body() {
    let (_temp, work, home) = fixture("mapper");
    std::fs::write(
        work.join(".flux/flows/mapped.flux"),
        "flow mapped(env: String, replicas: Number)\n  return {env: $env, replicas: $replicas}\n",
    )
    .unwrap();

    let mapped = run(
        &work,
        &home,
        &[
            "flow",
            "run",
            "mapped",
            "--map-inputs",
            "deploy three replicas to dev",
            "-m",
            "mock",
            "--yes",
        ],
        Some(r#"[{"env":"dev","replicas":3}]"#),
    );
    assert_success(&mapped, "model-mapped saved flow");
    assert!(
        String::from_utf8_lossy(&mapped.stdout).contains(r#"{"env":"dev","replicas":3}"#),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&mapped.stdout),
        String::from_utf8_lossy(&mapped.stderr)
    );

    std::fs::write(
        work.join(".flux/flows/mapped-bad.flux"),
        "flow mapped-bad(env: String)\n  write({path: \"body-ran.txt\", content: \"ran\"})\n  return $env\n",
    )
    .unwrap();
    let malformed = run(
        &work,
        &home,
        &[
            "flow",
            "run",
            "mapped-bad",
            "--map-inputs",
            "use dev",
            "-m",
            "mock",
            "--yes",
        ],
        Some("[]"),
    );
    assert!(
        !malformed.status.success(),
        "malformed mapper output must fail\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&malformed.stdout),
        String::from_utf8_lossy(&malformed.stderr)
    );
    assert!(
        !work.join("body-ran.txt").exists(),
        "the original flow body must not execute after mapper validation fails"
    );
}
