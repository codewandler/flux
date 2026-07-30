//! The `terminal-bench` adapter: real headroom for the self-improvement loop.
//!
//! flux is registered as a terminal-bench *custom agent* ([`crates/flux-eval/terminal_bench/flux_agent.py`]),
//! and `tb run` drives the Docker containers + grades (authoritative). This adapter shells out to
//! `tb run` for one task (one attempt) and parses its `results.json` into a [`RunResult`], so the
//! existing trials → [`CaseOutcome`](crate::metrics::CaseOutcome) → score path is unchanged.
//!
//! The binary the agent installs into each container is the **static musl** flux build
//! (`target/x86_64-unknown-linux-musl/release/flux`) — portable across task images. For the improve
//! loop, that musl binary must be rebuilt from the candidate source before the candidate eval (so the
//! benchmark measures the changed flux); trusted host configuration points `FLUX_EVAL_BINARY` at it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::Value;

use flux_core::{Error, Result, Usage};
use flux_system::{System, Workspace};

use crate::adapter::{BenchmarkAdapter, Filter, RunContext};
use crate::metrics::RunResult;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn env_truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Drives terminal-bench via its `tb` CLI with flux as a custom agent.
pub struct TerminalBenchAdapter {
    tasks: Vec<String>,
    dataset: String,
    tb_bin: String,
    agent_import_path: String,
    pythonpath: String,
    timeout_secs: u64,
    /// Per-task in-container agent timeout (tb `--global-agent-timeout-sec`) — bounds each flux run.
    agent_timeout_secs: u64,
    /// Rebuild the static musl binary in `prepare()` (so a candidate eval measures the worker's edits).
    rebuild: bool,
}

impl TerminalBenchAdapter {
    /// Build from an `eval_run` suite object. Executable and import paths are deliberately absent:
    /// the flux child comes from the trusted [`RunContext`], the terminal-bench executable comes from
    /// `FLUX_TERMINAL_BENCH_BINARY`, and the bundled agent import path is host-owned.
    pub fn from_params(params: &Value) -> Result<Self> {
        Self::from_params_with_env(params, |key| std::env::var(key).ok())
    }

    fn from_params_with_env(
        params: &Value,
        get_env: impl Fn(&str) -> Option<String>,
    ) -> Result<Self> {
        for field in [
            "flux_bin",
            "flux_binary",
            "tb_bin",
            "agent_import_path",
            "pythonpath",
            "dataset",
            "rebuild",
        ] {
            if params.get(field).is_some() {
                return Err(Error::Other(format!(
                    "terminal-bench: `{field}` is not a tool input; executable and import paths are selected by the trusted host"
                )));
            }
        }
        let tasks: Vec<String> = params
            .get("tasks")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let tb_bin = get_env("FLUX_TERMINAL_BENCH_BINARY")
            .filter(|path| !path.trim().is_empty())
            .unwrap_or_else(|| "tb".to_string());
        let dataset = get_env("FLUX_TERMINAL_BENCH_DATASET")
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| "terminal-bench-core".to_string());
        let rebuild = get_env("FLUX_TERMINAL_BENCH_REBUILD")
            .as_deref()
            .is_some_and(env_truthy);
        let cwd = std::env::current_dir()
            .map_err(|e| Error::Other(format!("terminal-bench: locate host workspace: {e}")))?;
        Ok(Self {
            tasks,
            dataset,
            tb_bin,
            agent_import_path: "flux_agent:FluxAgent".to_string(),
            pythonpath: cwd
                .join("crates/flux-eval/terminal_bench")
                .to_string_lossy()
                .into_owned(),
            timeout_secs: params
                .get("timeout_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(1800),
            agent_timeout_secs: params
                .get("agent_timeout_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(600),
            rebuild,
        })
    }

    /// Absolute path to the trusted-host flux binary the container agent installs.
    fn flux_binary_abs(ctx: &RunContext<'_>) -> String {
        let p = ctx.flux_bin;
        if p.is_absolute() {
            p.to_string_lossy().into_owned()
        } else {
            std::env::current_dir()
                .map(|c| c.join(p).to_string_lossy().to_string())
                .unwrap_or_else(|_| p.to_string_lossy().into_owned())
        }
    }

    fn task_argv(&self, task_id: &str, ctx: &RunContext<'_>, out: &std::path::Path) -> Vec<String> {
        vec![
            self.tb_bin.clone(),
            "run".into(),
            "--dataset".into(),
            self.dataset.clone(),
            "--task-id".into(),
            task_id.to_string(),
            "--n-attempts".into(),
            "1".into(),
            "--agent-import-path".into(),
            self.agent_import_path.clone(),
            "--model".into(),
            ctx.default_model.to_string(),
            "--agent-kwarg".into(),
            format!("flux_binary={}", Self::flux_binary_abs(ctx)),
            "--output-path".into(),
            out.to_string_lossy().to_string(),
            "--global-agent-timeout-sec".into(),
            self.agent_timeout_secs.to_string(),
            "--no-livestream".into(),
        ]
    }

    fn task_env(
        &self,
        model: &str,
        get_env: impl Fn(&str) -> Option<String>,
    ) -> Vec<(String, String)> {
        let home = get_env("HOME").unwrap_or_default();
        let path = format!(
            "{}/.local/bin:{}",
            home,
            get_env("PATH").unwrap_or_default()
        );
        let mut env = vec![
            ("PATH".into(), path),
            ("PYTHONPATH".into(), self.pythonpath.clone()),
        ];
        env.extend(crate::runner::provider_credential_env(model, get_env));
        env
    }
}

/// One parsed terminal-bench trial: pass-all, token counts, failure mode, and per-sub-check detail.
struct ParsedTrial {
    resolved: bool,
    input: u64,
    output: u64,
    failure: Option<String>,
    checks_passed: u32,
    checks_total: u32,
    failed_checks: Vec<String>,
}

/// Read tb's `results.json` (a `BenchmarkResults`) and pull this task's trial outcome, including the
/// per-sub-check `parser_results` map (for partial credit + a concrete failure breakdown).
fn parse_results(dir: &std::path::Path, task_id: &str) -> Option<ParsedTrial> {
    // tb writes `<output>/<run-id>/results.json`; search a couple of levels for it.
    let mut candidates = vec![dir.join("results.json")];
    // flux-allow-direct-io: parse terminal-bench output below the harness-generated result root;
    // this adapter owns that external tool protocol and model input cannot choose the root.
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if e.path().is_dir() {
                candidates.push(e.path().join("results.json"));
            }
        }
    }
    let path = candidates.into_iter().find(|p| p.exists())?;
    // flux-allow-direct-io: read the fixed results.json protocol file discovered below the
    // harness-generated terminal-bench output root.
    let json: Value = serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()?;
    let results = json.get("results")?.as_array()?;
    // Prefer the entry matching this task; else the first.
    let entry = results
        .iter()
        .find(|r| r.get("task_id").and_then(|v| v.as_str()) == Some(task_id))
        .or_else(|| results.first())?;
    let resolved = entry
        .get("is_resolved")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let input = entry
        .get("total_input_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let output = entry
        .get("total_output_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let failure = entry
        .get("failure_mode")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty() && *s != "none" && *s != "unset")
        .map(String::from);
    // Per-sub-check detail: `parser_results` maps each check name to "passed"/"failed".
    let (mut checks_passed, mut checks_total, mut failed_checks) = (0u32, 0u32, Vec::new());
    if let Some(pr) = entry.get("parser_results").and_then(|v| v.as_object()) {
        for (name, status) in pr {
            checks_total += 1;
            if status.as_str() == Some("passed") {
                checks_passed += 1;
            } else {
                failed_checks.push(name.clone());
            }
        }
    }
    Some(ParsedTrial {
        resolved,
        input,
        output,
        failure,
        checks_passed,
        checks_total,
        failed_checks,
    })
}

/// Strip ANSI/OSC escape sequences from a terminal byte stream so the digest is readable text.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                // CSI: ESC [ … final byte in @..~
                chars.next();
                while let Some(&n) = chars.peek() {
                    chars.next();
                    if ('@'..='~').contains(&n) {
                        break;
                    }
                }
            }
            Some(']') => {
                // OSC: ESC ] … terminated by BEL or ESC \
                chars.next();
                while let Some(&n) = chars.peek() {
                    chars.next();
                    if n == '\u{7}' {
                        break;
                    }
                    if n == '\u{1b}' {
                        if chars.peek() == Some(&'\\') {
                            chars.next();
                        }
                        break;
                    }
                }
            }
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
    out
}

/// Recursively find a file named `name` under `dir` (bounded depth — tb nests
/// `<run>/<task>/<trial>/sessions/agent.cast`).
fn find_file(dir: &std::path::Path, name: &str, depth: usize) -> Option<std::path::PathBuf> {
    if depth == 0 {
        return None;
    }
    let mut subdirs = Vec::new();
    // flux-allow-direct-io: bounded traversal of the harness-generated terminal-bench result root
    // to recover its fixed agent.cast protocol artifact.
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() {
            subdirs.push(p);
        } else if p.file_name().and_then(|n| n.to_str()) == Some(name) {
            return Some(p);
        }
    }
    subdirs
        .into_iter()
        .find_map(|d| find_file(&d, name, depth - 1))
}

/// Decode an asciinema v2 cast (JSONL: header line, then `[time, kind, data]`) into plain text — the
/// `"o"` output events, ANSI stripped — and return the last `max_chars`, so the reviewer sees what the
/// agent actually did in the container (commands it ran, errors like `node: not found`, timeouts).
fn decode_cast_tail(path: &std::path::Path, max_chars: usize) -> Option<String> {
    // flux-allow-direct-io: decode the fixed agent.cast protocol artifact found only below the
    // harness-generated terminal-bench output root.
    let content = std::fs::read_to_string(path).ok()?;
    let mut out = String::new();
    for line in content.lines().skip(1) {
        if let Ok(Value::Array(ev)) = serde_json::from_str::<Value>(line) {
            if ev.get(1).and_then(|v| v.as_str()) == Some("o") {
                if let Some(s) = ev.get(2).and_then(|v| v.as_str()) {
                    out.push_str(s);
                }
            }
        }
    }
    let out = strip_ansi(&out);
    let n = out.chars().count();
    let tail: String = if n > max_chars {
        out.chars().skip(n - max_chars).collect()
    } else {
        out
    };
    let tail = tail.trim().to_string();
    (!tail.is_empty()).then_some(tail)
}

#[async_trait]
impl BenchmarkAdapter for TerminalBenchAdapter {
    fn name(&self) -> &str {
        "terminal-bench"
    }

    async fn prepare(&self, _ctx: &RunContext<'_>) -> Result<()> {
        if !self.rebuild {
            return Ok(());
        }
        // Rebuild the static musl binary from the current (candidate) source so the container agent
        // installs the worker's edits, not a stale binary.
        let cwd = std::env::current_dir().map_err(|e| Error::Other(e.to_string()))?;
        // Deliberately unsandboxed: this is a host-side `cargo build` of the flux-cli musl binary that
        // the terminal-bench harness then installs into the task container. It drives the build
        // toolchain (rustc/cargo, crate-fetch network, the host's `~/.cargo`/`target` caches) — not
        // model work — and the real isolation boundary is the *task container* the built binary runs
        // in, not this build step. Confining it (the sandbox masks `/run`, restricts writes/network)
        // would break the toolchain, and `require` never reaches here (eval driver, not an agent spawn).
        let sys = System::new(
            Workspace::new(&cwd)
                .map_err(|e| Error::Other(format!("musl rebuild workspace: {e}")))?,
        );
        let argv: Vec<String> = [
            "cargo",
            "build",
            "--release",
            "-p",
            "flux-cli",
            "--target",
            "x86_64-unknown-linux-musl",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let out = sys
            .run_with_env(
                &argv,
                &crate::runner::toolchain_env(),
                Duration::from_secs(1800),
            )
            .await?;
        if out.exit_code != 0 {
            let tail: String = out
                .stderr
                .lines()
                .rev()
                .take(6)
                .collect::<Vec<_>>()
                .join(" | ");
            return Err(Error::Other(format!(
                "musl rebuild failed (exit {}): {tail}",
                out.exit_code
            )));
        }
        Ok(())
    }

    fn list_tasks(&self, filter: &Filter) -> Result<Vec<String>> {
        // terminal-bench is heavy (a Docker image per task), so we require explicit task ids rather
        // than auto-listing the whole dataset.
        let ids = if !filter.ids.is_empty() {
            filter.select(&filter.ids.clone())
        } else {
            filter.select(&self.tasks)
        };
        if ids.is_empty() {
            return Err(Error::Other(
                "terminal-bench: specify task ids via the suite `tasks` array or eval_run `tasks`"
                    .to_string(),
            ));
        }
        Ok(ids)
    }

    async fn run_task(&self, task_id: &str, ctx: &RunContext<'_>) -> Result<RunResult> {
        let started = Instant::now();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let out = std::env::temp_dir().join(format!("flux-tbench-{}-{n}", std::process::id()));
        // flux-allow-direct-io: terminal-bench adapter owns this unpredictable process-scoped output
        // root before its System can be constructed; model input cannot choose the path.
        std::fs::create_dir_all(&out).map_err(|e| Error::Other(e.to_string()))?;

        let argv = self.task_argv(task_id, ctx, &out);

        // tb needs PATH (to find `tb`/`docker`), PYTHONPATH (to import the flux agent), and provider
        // creds. SAFE_ENV forwards PATH/HOME; augment PATH with ~/.local/bin (uv tool installs there).
        let env = self.task_env(ctx.default_model, |key| std::env::var(key).ok());

        // tb runs in the repo/worktree root (it manages its own dataset cache + Docker). Honour the
        // host's resolved sandbox posture: a `require` deployment must fail closed if its sandbox
        // cannot expose the Docker boundary terminal-bench needs, not silently turn confinement off.
        let cwd = std::env::current_dir().map_err(|e| Error::Other(e.to_string()))?;
        let sys = System::new(
            Workspace::new(&cwd).map_err(|e| Error::Other(format!("tb workspace: {e}")))?,
        )
        .with_sandbox(flux_system::sandbox::Sandbox::resolve(
            flux_system::sandbox::SandboxSettings::from_env(),
        ));
        let run = sys
            .run_with_env(&argv, &env, Duration::from_secs(self.timeout_secs))
            .await;
        let wall_ms = started.elapsed().as_millis() as u64;

        match run {
            Err(e) => {
                let msg = e.to_string();
                let mut r = RunResult::failed(task_id, wall_ms, format!("tb run: {msg}"));
                r.timed_out = msg.contains("timed out");
                Ok(r)
            }
            Ok(output) => {
                // The agent's in-container session recording (the commands it ran, the errors it hit)
                // — fed to the reviewer so it can diagnose in-container friction pass/fail can't show.
                let transcript =
                    find_file(&out, "agent.cast", 7).and_then(|p| decode_cast_tail(&p, 3000));
                if let Some(p) = parse_results(&out, task_id) {
                    let tokens = if p.input + p.output > 0 {
                        Some(Usage {
                            input_tokens: p.input,
                            output_tokens: p.output,
                            ..Default::default()
                        })
                    } else {
                        None
                    };
                    Ok(RunResult {
                        task_id: task_id.to_string(),
                        valid: true,
                        passed: p.resolved,
                        checks_passed: p.checks_passed,
                        checks_total: p.checks_total,
                        failed_checks: p.failed_checks,
                        iterations: 0,
                        tool_calls: 0,
                        tool_errors: 0,
                        tokens,
                        wall_ms,
                        session_id: None,
                        session_db: None,
                        flow_db: None,
                        timed_out: false,
                        note: p.failure,
                        transcript,
                    })
                } else {
                    // No parseable results — surface tb's tail for debugging.
                    let tail: String = output
                        .stdout
                        .lines()
                        .chain(output.stderr.lines())
                        .rev()
                        .take(8)
                        .collect::<Vec<_>>()
                        .join(" | ");
                    let mut r = RunResult::failed(
                        task_id,
                        wall_ms,
                        format!(
                            "tb run: no results.json parsed (exit {}): {tail}",
                            output.exit_code
                        ),
                    );
                    r.transcript = transcript;
                    Ok(r)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tokio_util::sync::CancellationToken;

    use super::*;

    #[test]
    fn from_params_rejects_model_controlled_program_and_dataset_fields() {
        for field in [
            "flux_bin",
            "flux_binary",
            "tb_bin",
            "agent_import_path",
            "pythonpath",
            "dataset",
            "rebuild",
        ] {
            let mut params = serde_json::json!({"tasks": ["hello-world"]});
            params
                .as_object_mut()
                .expect("test params are an object")
                .insert(
                    field.to_string(),
                    Value::String("/tmp/attacker".to_string()),
                );
            let error = TerminalBenchAdapter::from_params_with_env(&params, |_| None)
                .err()
                .expect("legacy model-controlled field must be rejected");
            assert!(error.to_string().contains(field), "{field}: {error}");
        }
    }

    #[test]
    fn credentialed_command_uses_only_trusted_host_program_paths() {
        let a = TerminalBenchAdapter::from_params_with_env(
            &serde_json::json!({"tasks": ["hello-world"]}),
            |key| match key {
                "FLUX_TERMINAL_BENCH_BINARY" => Some("/trusted/tb".to_string()),
                "FLUX_TERMINAL_BENCH_DATASET" => Some("trusted-suite".to_string()),
                "FLUX_TERMINAL_BENCH_REBUILD" => Some("true".to_string()),
                _ => None,
            },
        )
        .unwrap();
        let cancel = CancellationToken::new();
        let ctx = RunContext {
            flux_bin: Path::new("/trusted/flux"),
            default_model: "openai/gpt-test",
            cancel: &cancel,
            watch: false,
        };
        let argv = a.task_argv("hello-world", &ctx, Path::new("/tmp/results"));
        assert_eq!(argv.first().map(String::as_str), Some("/trusted/tb"));
        assert!(argv.iter().any(|arg| arg == "flux_binary=/trusted/flux"));
        assert!(argv.iter().any(|arg| arg == "trusted-suite"));
        assert!(argv.iter().any(|arg| arg == "flux_agent:FluxAgent"));
        assert!(a.rebuild);

        let env = a.task_env("openai/gpt-test", |key| match key {
            "HOME" => Some("/trusted/home".to_string()),
            "PATH" => Some("/trusted/path".to_string()),
            "OPENAI_API_KEY" => Some("sentinel-provider-key".to_string()),
            _ => None,
        });
        assert!(env
            .iter()
            .any(|(key, value)| { key == "OPENAI_API_KEY" && value == "sentinel-provider-key" }));
        assert!(argv.iter().all(|arg| !arg.contains("attacker")));
    }

    #[test]
    fn terminal_bench_defaults_are_host_owned() {
        let a = TerminalBenchAdapter::from_params_with_env(
            &serde_json::json!({"tasks": ["hello-world"]}),
            |_| None,
        )
        .unwrap();
        assert_eq!(a.name(), "terminal-bench");
        assert_eq!(a.tb_bin, "tb");
        assert_eq!(a.dataset, "terminal-bench-core");
        assert_eq!(a.agent_import_path, "flux_agent:FluxAgent");
        assert!(!a.rebuild);
    }

    #[test]
    fn parse_results_reads_is_resolved_and_tokens() {
        let dir = std::env::temp_dir().join(format!("tb-parse-test-{}", std::process::id()));
        let run = dir.join("2026-run-abc");
        std::fs::create_dir_all(&run).unwrap();
        std::fs::write(
            run.join("results.json"),
            serde_json::json!({
                "results": [
                    {"task_id": "hello-world", "is_resolved": true,
                     "total_input_tokens": 1200, "total_output_tokens": 300,
                     "failure_mode": "none"}
                ]
            })
            .to_string(),
        )
        .unwrap();
        let p = parse_results(&dir, "hello-world").unwrap();
        assert!(p.resolved);
        assert_eq!(p.input, 1200);
        assert_eq!(p.output, 300);
        assert!(p.failure.is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parse_results_extracts_parser_results_partial_credit() {
        let dir = std::env::temp_dir().join(format!("tb-parse-sub-{}", std::process::id()));
        let run = dir.join("run");
        std::fs::create_dir_all(&run).unwrap();
        // A near-miss like today's fibonacci-server candidate: 5 of 6 sub-checks pass.
        std::fs::write(
            run.join("results.json"),
            serde_json::json!({
                "results": [
                    {"task_id": "fibonacci-server", "is_resolved": false,
                     "parser_results": {
                         "test_server_running": "passed",
                         "test_fibonacci_endpoint_small_numbers": "passed",
                         "test_fibonacci_large_number": "passed",
                         "test_missing_parameter": "passed",
                         "test_non_integer_parameter": "passed",
                         "test_negative_number": "failed"
                     }}
                ]
            })
            .to_string(),
        )
        .unwrap();
        let p = parse_results(&dir, "fibonacci-server").unwrap();
        assert!(!p.resolved);
        assert_eq!(p.checks_total, 6);
        assert_eq!(p.checks_passed, 5);
        assert_eq!(p.failed_checks, vec!["test_negative_number".to_string()]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn decode_cast_tail_extracts_output_and_strips_ansi() {
        let dir = std::env::temp_dir().join(format!("tb-cast-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cast = dir.join("agent.cast");
        let lines = [
            r#"{"version":2,"width":80,"height":24}"#,
            r#"[0.1,"o","\u001b[32mnode: not found\u001b[0m\r\n"]"#,
            r#"[0.2,"i","ignored keystrokes"]"#,
            r#"[0.3,"o","python3 server.py\r\n"]"#,
        ];
        std::fs::write(&cast, lines.join("\n")).unwrap();
        let t = decode_cast_tail(&cast, 1000).unwrap();
        assert!(t.contains("node: not found"), "got: {t:?}");
        assert!(t.contains("python3 server.py"));
        assert!(!t.contains("ignored keystrokes")); // "i" (input) events are skipped
        assert!(!t.contains('\u{1b}')); // ANSI stripped
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn find_file_locates_nested_cast() {
        let dir = std::env::temp_dir().join(format!("tb-find-{}", std::process::id()));
        let nested = dir.join("run/task/trial/sessions");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("agent.cast"), "x").unwrap();
        assert!(find_file(&dir, "agent.cast", 7).is_some());
        assert!(find_file(&dir, "missing.cast", 7).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn container_agent_enables_the_shell_group() {
        // I-04: the container agent must run flux with the `shell` group enabled — a terminal-bench
        // container is a disposable task sandbox whose whole point is terminal work, and without
        // FLUX_ENABLE_BASH the agent writes files it can never execute/start (every check that
        // needs a running process fails). Pins the bundled python agent, which tb imports live via
        // PYTHONPATH, so a regression here silently depresses every containerized eval number.
        let py = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/terminal_bench/flux_agent.py"
        ))
        .expect("bundled flux_agent.py exists");
        assert!(
            py.contains("FLUX_ENABLE_BASH"),
            "flux_agent.py must enable the shell group for the in-container flux run (I-04)"
        );
    }
}
