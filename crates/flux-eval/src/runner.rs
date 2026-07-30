//! Run one local benchmark task: materialize a workspace, drive flux headlessly in it, grade the
//! success criterion **outside** the agent, and recover metrics from the child's isolated session log.
//!
//! Isolation: each task gets a fresh temp workspace (the agent's cwd) and a private `HOME`
//! (`<workdir>/.home`) so the child's `~/.flux/sessions.db` never collides with the parent's or with
//! other tasks. The criterion is graded through a [`System`] rooted at the workspace — argv-only, no
//! shell — so the agent can't "pass" by tampering with its own grader.

use std::path::Path;
use std::time::{Duration, Instant};

use regex::Regex;

use flux_core::{Error, Message, Result, Usage};
use flux_events::EventStore;
use flux_system::{System, Workspace};

use flux_flow::ast::RunEvent;

use crate::adapter::RunContext;
use crate::metrics::{iterations_from_messages, metrics_from_events, RunResult};
use crate::spec::{Criterion, SeedFile, Setup, TaskSpec};

use crate::util::unique_temp_dir;

const PROVIDER_CREDENTIAL_ENV: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "OPENROUTER_API_KEY",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AWS_REGION",
    "AWS_DEFAULT_REGION",
    "FLUX_SECRET",
];

/// Copy only the environment material needed by the selected provider. OAuth-backed and local
/// providers receive no raw credential environment; their own configured credential source is
/// responsible for authentication.
pub(crate) fn provider_credential_env(
    model: &str,
    get_env: impl Fn(&str) -> Option<String>,
) -> Vec<(String, String)> {
    let provider = model
        .split_once('/')
        .map(|(provider, _)| provider)
        .unwrap_or_else(|| match model {
            "mock" => "mock",
            "ollama" => "ollama",
            _ => "anthropic",
        });
    let keys: &[&str] = match provider {
        "anthropic" => &["ANTHROPIC_API_KEY"],
        "openai" => &["OPENAI_API_KEY"],
        "openrouter" => &["OPENROUTER_API_KEY"],
        "aws" => &[
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
            "AWS_REGION",
            "AWS_DEFAULT_REGION",
        ],
        _ => &[],
    };
    keys.iter()
        .filter_map(|key| get_env(key).map(|value| ((*key).to_string(), value)))
        .collect()
}

/// Append a task fixture's own `env` to the child's environment, minus the keys a fixture is not
/// allowed to speak for.
///
/// Two refusals, both because this env lands in `build_command`'s **caller-override** slot, which is
/// applied last and wins:
/// - provider credentials, so a fixture cannot smuggle a second provider's key or flux's host
///   secret into the child — authentication material comes solely from the selected-provider
///   allow-list;
/// - the sandbox posture (C-282). The harness resolves a posture and `sandbox::posture_env` forwards
///   it *before* this slot, so a fixture naming `FLUX_SANDBOX=off` would land after it and hand the
///   child `flux-cli`'s kill switch — which beats the child's own `[sandbox] require` and C-262's
///   unattended fail-closed profile. A benchmark task has no business moving the harness's
///   confinement in either direction, so the keys are dropped rather than honored. Filtered against
///   `flux_system::sandbox::POSTURE_ENV_KEYS` so the set cannot drift from what is actually
///   forwarded.
fn extend_task_env(
    env: &mut Vec<(String, String)>,
    task_env: &std::collections::BTreeMap<String, String>,
) {
    env.extend(
        task_env
            .iter()
            .filter(|(key, _)| {
                !PROVIDER_CREDENTIAL_ENV.contains(&key.as_str())
                    && !flux_system::sandbox::is_posture_env_key(key)
            })
            .map(|(key, value)| (key.clone(), value.clone())),
    );
}

fn io_err(e: std::io::Error) -> Error {
    Error::Other(e.to_string())
}

/// Reject seed paths that would escape the workspace (absolute or `..`).
fn safe_rel(path: &str) -> Result<()> {
    if Path::new(path).is_absolute() || path.split('/').any(|c| c == "..") {
        return Err(Error::Other(format!("unsafe seed path {path:?}")));
    }
    Ok(())
}

fn write_seed(workdir: &Path, f: &SeedFile) -> Result<()> {
    safe_rel(&f.path)?;
    let dest = workdir.join(&f.path);
    if let Some(parent) = dest.parent() {
        // flux-allow-direct-io: trusted benchmark fixture materialization into the freshly generated
        // eval temp root after safe_rel rejects absolute paths and parent traversal.
        std::fs::create_dir_all(parent).map_err(io_err)?;
    }
    // flux-allow-direct-io: trusted benchmark fixture materialization confined by safe_rel to the
    // freshly generated eval temp root; the agent never supplies seed paths or contents.
    std::fs::write(&dest, &f.content).map_err(io_err)
}

fn copy_dir_recursive(from: &Path, to: &Path) -> Result<()> {
    // flux-allow-direct-io: host-authored benchmark setup copies a trusted fixture tree into the
    // unpredictable eval temp root before the model starts; this is harness provisioning.
    for entry in std::fs::read_dir(from).map_err(io_err)? {
        let entry = entry.map_err(io_err)?;
        let src = entry.path();
        let dest = to.join(entry.file_name());
        if src.is_dir() {
            // flux-allow-direct-io: recursive destination remains below the harness-owned eval temp
            // root and is derived only from host-authored fixture entries.
            std::fs::create_dir_all(&dest).map_err(io_err)?;
            copy_dir_recursive(&src, &dest)?;
        } else {
            // flux-allow-direct-io: host-authored fixture copy into the harness-owned eval temp root;
            // neither endpoint is selected by the evaluated model.
            std::fs::copy(&src, &dest).map_err(io_err)?;
        }
    }
    Ok(())
}

/// Materialize a task's [`Setup`] into `workdir`.
fn materialize(setup: &Setup, workdir: &Path) -> Result<()> {
    match setup {
        Setup::Empty => Ok(()),
        Setup::Files { files } => {
            for f in files {
                write_seed(workdir, f)?;
            }
            Ok(())
        }
        Setup::Copy { from } => {
            let src = Path::new(from);
            if !src.is_dir() {
                return Err(Error::Other(format!(
                    "copy source {from:?} is not a directory"
                )));
            }
            copy_dir_recursive(src, workdir)
        }
        Setup::GitRef { .. } => Err(Error::Other(
            "the local adapter does not support `git_ref` setup; use an external benchmark adapter"
                .to_string(),
        )),
    }
}

/// Load the most-recent session from an isolated session store, returning its id and message log.
fn load_latest_session(db: &Path) -> Option<(Option<String>, Vec<Message>)> {
    if !db.exists() {
        return None;
    }
    let store = EventStore::open(db).ok()?;
    let id = store.latest_session().ok().flatten();
    let msgs = match &id {
        Some(i) => store.conversation(i).unwrap_or_default(),
        None => Vec::new(),
    };
    Some((id, msgs))
}

/// Load a session's RunEvent trace from the isolated unified event store (the source of
/// tool-call/error signal).
fn load_events(events_db: &Path, session_id: &str) -> Vec<RunEvent> {
    if !events_db.exists() {
        return Vec::new();
    }
    EventStore::open(events_db)
        .ok()
        .and_then(|s| s.run_trace(session_id).ok())
        .unwrap_or_default()
}

/// Sum the per-turn token `usage` recorded in a session's `TurnEnded` telemetry. Returns `None` when
/// no turn carried usage (an older binary, or a provider that reported none), so a token-less run keeps
/// `tokens: None` rather than a misleading zero. Fields are summed across turns — each turn's prompt is
/// billed independently — so `total()` reflects the run's real token cost (the `mean_tokens`
/// score tiebreaker).
fn load_usage(events_db: &Path, session_id: &str) -> Option<Usage> {
    if !events_db.exists() {
        return None;
    }
    let store = EventStore::open(events_db).ok()?;
    let turns = store.turns(session_id).ok()?;
    let mut acc = Usage::default();
    let mut any = false;
    for t in turns {
        if let Some(u) = t.usage {
            acc.input_tokens += u.input_tokens;
            acc.output_tokens += u.output_tokens;
            acc.cache_read_input_tokens += u.cache_read_input_tokens;
            acc.cache_creation_input_tokens += u.cache_creation_input_tokens;
            any = true;
        }
    }
    any.then_some(acc)
}

/// Rust toolchain env to forward into the scrubbed child / grader: without `RUSTUP_HOME` (and the
/// isolated `HOME` lacking `~/.rustup`), rustup reports "no default toolchain configured" and any
/// `cargo` criterion fails spuriously. Reads the vars if set, else defaults to `$HOME/.{rustup,cargo}`.
pub(crate) fn toolchain_env() -> Vec<(String, String)> {
    let mut out = Vec::new();
    let home = std::env::var("HOME").ok();
    for (key, sub) in [("RUSTUP_HOME", ".rustup"), ("CARGO_HOME", ".cargo")] {
        if let Ok(v) = std::env::var(key) {
            out.push((key.to_string(), v));
        } else if let Some(def) = home.as_ref().map(|h| format!("{h}/{sub}")) {
            if Path::new(&def).exists() {
                out.push((key.to_string(), def));
            }
        }
    }
    out
}

/// Grade a criterion in the (already-finished) workspace. Reads/exec go through `sys`. Public so the
/// `grade` op (and any evidence-based flow) can reuse the exact same pass/fail check the eval harness
/// uses — one grading implementation, no divergence.
pub async fn grade(c: &Criterion, sys: &System) -> Result<bool> {
    match c {
        Criterion::Command {
            run,
            expect_exit,
            stdout_equals,
            stdout_contains,
            stdout_regex,
        } => {
            let argv: Vec<String> = run.split_whitespace().map(String::from).collect();
            if argv.is_empty() {
                return Ok(false);
            }
            // Forward the toolchain env so `cargo`/`rustup` criteria work in the scrubbed env.
            let out = sys
                .run_with_env(&argv, &toolchain_env(), Duration::from_secs(180))
                .await?;
            let mut ok = out.exit_code == *expect_exit;
            if let Some(eq) = stdout_equals {
                ok &= out.stdout.trim() == eq;
            }
            if let Some(sub) = stdout_contains {
                ok &= out.stdout.contains(sub.as_str());
            }
            if let Some(re) = stdout_regex {
                let re = Regex::new(re)
                    .map_err(|e| Error::Other(format!("bad criterion stdout_regex {re:?}: {e}")))?;
                ok &= re.is_match(&out.stdout);
            }
            Ok(ok)
        }
        Criterion::FileContent {
            path,
            equals,
            contains,
            regex,
        } => {
            let content = match sys.read_file(path).await {
                Ok(c) => c,
                Err(_) => return Ok(false), // missing / unreadable / non-UTF-8 → fail
            };
            let mut ok = true;
            if let Some(eq) = equals {
                ok &= &content == eq;
            }
            if let Some(sub) = contains {
                ok &= content.contains(sub);
            }
            if let Some(re) = regex {
                let re = Regex::new(re)
                    .map_err(|e| Error::Other(format!("bad criterion regex {re:?}: {e}")))?;
                ok &= re.is_match(&content);
            }
            Ok(ok)
        }
        Criterion::All { of } => {
            for sub in of {
                if !Box::pin(grade(sub, sys)).await? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
    }
}

/// Run one local task end-to-end.
pub async fn run_local_task(spec: &TaskSpec, ctx: &RunContext<'_>) -> Result<RunResult> {
    let started = Instant::now();

    if ctx.cancel.is_cancelled() {
        return Ok(RunResult::failed(&spec.id, 0, "cancelled before start"));
    }

    let workdir = unique_temp_dir("flux-eval-task").map_err(io_err)?;
    materialize(&spec.setup, &workdir)?;
    let home = workdir.join(".home");
    // flux-allow-direct-io: private HOME is harness infrastructure below the unpredictable eval
    // temp root and must exist before the sandboxed child flux process is launched.
    std::fs::create_dir_all(&home).map_err(io_err)?;

    let model = spec
        .model
        .clone()
        .unwrap_or_else(|| ctx.default_model.to_string());
    let provider_env = provider_credential_env(&model, |key| std::env::var(key).ok());

    // Attach the environment's sandbox posture without pulling in FLUX_ADD_DIRS/FLUX_ALLOW_ALL,
    // which would defeat the isolated eval workspace. The eval child uses the ordinary sandboxed
    // process path, so `require` and sandbox-network policy apply honestly to the whole child.
    let sys = System::new(
        Workspace::new(&workdir)
            .map_err(|e| Error::Other(format!("eval workspace {}: {e}", workdir.display())))?,
    )
    .with_sandbox(flux_system::sandbox::Sandbox::resolve(
        flux_system::sandbox::SandboxSettings::from_env(),
    ));

    let argv = vec![
        ctx.flux_bin.to_string_lossy().to_string(),
        "run".to_string(),
        "--yes".to_string(),
        "-m".to_string(),
        model,
        "-p".to_string(),
        spec.prompt.clone(),
    ];
    let mut env: Vec<(String, String)> =
        vec![("HOME".to_string(), home.to_string_lossy().to_string())];
    env.extend(provider_env);
    // Rust toolchain (so the child's own `cargo`/`rustup` tools work under the isolated HOME).
    env.extend(toolchain_env());
    // The sandbox posture is NOT appended here. `sys` above carries the resolved `Sandbox`, and the
    // guarded spawn renders the posture from it (`sandbox::posture_env`) — one implementation of
    // that decision, reading the resolved sandbox rather than `std::env`, which `System::with_sandbox`
    // exists to diverge from. `extend_task_env` drops the posture keys so `spec.env` cannot land in
    // the caller-override slot and replace it (C-282).
    extend_task_env(&mut env, &spec.env);
    // In watch mode, reveal authored-loop stages and evidence events.
    if ctx.watch {
        env.push(("FLUX_SHOW_LOOP".to_string(), "1".to_string()));
    }

    let run = if ctx.watch {
        eprintln!("\n── {} ──", spec.id);
        sys.run_with_env_streamed(&argv, &env, Duration::from_secs(spec.timeout_secs))
            .await
    } else {
        sys.run_with_env(&argv, &env, Duration::from_secs(spec.timeout_secs))
            .await
    };
    let wall_ms = started.elapsed().as_millis() as u64;

    let mut timed_out = false;
    let mut note = None;
    if let Err(e) = &run {
        let msg = e.to_string();
        if msg.contains("timed out") {
            timed_out = true;
        }
        note = Some(msg);
    }

    // Messages and the RunEvent trace now share one unified log (`~/.flux/events.db`).
    let events_db = home.join(".flux").join("events.db");
    let (session_id, messages) = load_latest_session(&events_db).unwrap_or((None, Vec::new()));
    let iterations = iterations_from_messages(&messages);
    let events = match &session_id {
        Some(id) => load_events(&events_db, id),
        None => Vec::new(),
    };
    let (tool_calls, tool_errors) = metrics_from_events(&events);
    let tokens = session_id
        .as_deref()
        .and_then(|id| load_usage(&events_db, id));

    let (passed, valid) = if timed_out {
        (false, true)
    } else {
        match grade(&spec.criterion, &sys).await {
            Ok(p) => (p, true),
            Err(e) => {
                if note.is_none() {
                    note = Some(format!("grade error: {e}"));
                }
                (false, false)
            }
        }
    };

    Ok(RunResult {
        task_id: spec.id.clone(),
        valid,
        passed,
        // The local adapter grades a task as a single pass/fail (no sub-checks); partial credit
        // falls back to this binary outcome in aggregation.
        checks_passed: 0,
        checks_total: 0,
        failed_checks: Vec::new(),
        iterations,
        tool_calls,
        tool_errors,
        tokens,
        wall_ms,
        session_id,
        session_db: Some(events_db.clone()),
        flow_db: Some(events_db),
        timed_out,
        note,
        // The local adapter keeps the full RunEvent trace (flow_db) for deterministic mining, so it
        // doesn't need a separate session digest here.
        transcript: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_system() -> (PathBuf, System) {
        let dir = unique_temp_dir("flux-eval-runner-test").unwrap();
        let sys = System::new(Workspace::new(&dir).unwrap());
        (dir, sys)
    }

    /// C-282: a task fixture's `env` is a **caller override**, and the guarded spawn applies those
    /// *after* `sandbox::posture_env` — so a benchmark that names `FLUX_SANDBOX=off` would land
    /// last and hand the eval child `flux-cli`'s kill switch, which beats the child's own
    /// `[sandbox] require` and C-262's unattended fail-closed profile.
    ///
    /// `run_local_task` used to defend this by re-reading the ambient posture out of `std::env` and
    /// appending it afterwards. That is a hand-rolled copy of a decision with one correct
    /// implementation, and — read from the environment rather than from the resolved `Sandbox` —
    /// it disagrees with a pinned posture in exactly the way C-276's first attempt was reworked
    /// for. A fixture has no legitimate reason to move the harness's posture in either direction,
    /// so the keys are refused here instead.
    #[test]
    fn a_task_fixture_may_not_name_the_eval_childs_sandbox_posture() {
        let mut env = Vec::new();
        extend_task_env(
            &mut env,
            &std::collections::BTreeMap::from([
                ("FLUX_SANDBOX".to_string(), "off".to_string()),
                ("FLUX_SANDBOX_NET".to_string(), "1".to_string()),
                (
                    "FLUX_BWRAP_BIN".to_string(),
                    "/nonexistent/other-bwrap".to_string(),
                ),
                ("TASK_FIXTURE".to_string(), "kept".to_string()),
            ]),
        );
        assert!(
            !env.iter()
                .any(|(key, _)| key.starts_with("FLUX_SANDBOX") || key == "FLUX_BWRAP_BIN"),
            "a benchmark fixture must not be able to downgrade the posture the harness resolved: \
             {env:?}"
        );
        assert!(
            env.contains(&("TASK_FIXTURE".to_string(), "kept".to_string())),
            "only the posture keys are dropped — a task's own env is the field's whole point: \
             {env:?}"
        );
    }

    #[test]
    fn eval_child_receives_only_the_selected_provider_credential() {
        let values = std::collections::HashMap::from([
            ("ANTHROPIC_API_KEY", "anthropic-sentinel"),
            ("OPENAI_API_KEY", "openai-sentinel"),
            ("OPENROUTER_API_KEY", "openrouter-sentinel"),
            ("FLUX_SECRET", "flux-sentinel"),
        ]);
        let env = provider_credential_env("openai/gpt-5", |key| {
            values.get(key).map(ToString::to_string)
        });
        assert_eq!(
            env,
            vec![("OPENAI_API_KEY".to_string(), "openai-sentinel".to_string())]
        );
        assert!(!env.iter().any(|(key, _)| key == "FLUX_SECRET"));
    }

    #[test]
    fn bare_anthropic_alias_receives_only_anthropic_key() {
        let env = provider_credential_env("sonnet", |key| Some(format!("{key}-value")));
        assert_eq!(env.len(), 1);
        assert_eq!(env[0].0, "ANTHROPIC_API_KEY");
    }

    #[test]
    fn task_fixture_cannot_reintroduce_host_or_unrelated_provider_secrets() {
        let task_env = std::collections::BTreeMap::from([
            ("SAFE_FIXTURE".to_string(), "yes".to_string()),
            ("OPENAI_API_KEY".to_string(), "unrelated".to_string()),
            ("FLUX_SECRET".to_string(), "host-secret".to_string()),
        ]);
        let mut env = Vec::new();
        extend_task_env(&mut env, &task_env);
        assert_eq!(env, vec![("SAFE_FIXTURE".to_string(), "yes".to_string())]);
    }

    #[test]
    fn model_reachable_eval_runner_has_no_sandbox_exemption() {
        let source = include_str!("runner.rs");
        let captured = ["run_with_env", "_exempt("].concat();
        let streamed = ["run_with_env_streamed", "_exempt("].concat();
        assert!(!source.contains(&captured));
        assert!(!source.contains(&streamed));
    }

    #[test]
    fn load_usage_sums_token_tally_across_turns() {
        let dir = unique_temp_dir("flux-eval-usage-test").unwrap();
        let db = dir.join("events.db");
        let id = {
            let store = EventStore::open(&db).unwrap();
            let id = store.create_session("m").unwrap();
            let t1 = store.begin_turn(&id, "task", "m").unwrap();
            store
                .end_turn(
                    &id,
                    t1,
                    "accepted",
                    1,
                    "a",
                    Some(Usage {
                        input_tokens: 100,
                        output_tokens: 20,
                        ..Default::default()
                    }),
                )
                .unwrap();
            let t2 = store.begin_turn(&id, "more", "m").unwrap();
            store
                .end_turn(
                    &id,
                    t2,
                    "accepted",
                    1,
                    "b",
                    Some(Usage {
                        input_tokens: 30,
                        output_tokens: 5,
                        ..Default::default()
                    }),
                )
                .unwrap();
            id
        };
        // Summed across both turns: in 130, out 25 → total 155 (each turn's prompt is billed).
        let usage = load_usage(&db, &id).expect("usage recorded");
        assert_eq!(usage.input_tokens, 130);
        assert_eq!(usage.output_tokens, 25);
        assert_eq!(usage.total(), 155);

        // A session with no recorded usage reads back as `None`, not a misleading zero.
        let db2 = dir.join("events2.db");
        let id2 = {
            let store = EventStore::open(&db2).unwrap();
            let id2 = store.create_session("m").unwrap();
            let t = store.begin_turn(&id2, "task", "m").unwrap();
            store.end_turn(&id2, t, "accepted", 1, "a", None).unwrap();
            id2
        };
        assert!(load_usage(&db2, &id2).is_none());
    }

    #[test]
    fn materialize_writes_seed_files_and_rejects_escape() {
        let dir = unique_temp_dir("flux-eval-mat-test").unwrap();
        materialize(
            &Setup::Files {
                files: vec![SeedFile {
                    path: "src/lib.rs".into(),
                    content: "fn main() {}".into(),
                }],
            },
            &dir,
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("src/lib.rs")).unwrap(),
            "fn main() {}"
        );

        let bad = materialize(
            &Setup::Files {
                files: vec![SeedFile {
                    path: "../escape.txt".into(),
                    content: "x".into(),
                }],
            },
            &dir,
        );
        assert!(bad.is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn grade_file_content_matches_and_misses() {
        let (dir, sys) = temp_system();
        sys.write_file("COUNT.txt", "3").await.unwrap();

        assert!(grade(
            &Criterion::FileContent {
                path: "COUNT.txt".into(),
                equals: Some("3".into()),
                contains: None,
                regex: Some(r"^\s*3\s*$".into()),
            },
            &sys
        )
        .await
        .unwrap());

        // Wrong expectation → fail.
        assert!(!grade(
            &Criterion::FileContent {
                path: "COUNT.txt".into(),
                equals: Some("4".into()),
                contains: None,
                regex: None,
            },
            &sys
        )
        .await
        .unwrap());

        // Missing file → fail (not error).
        assert!(!grade(
            &Criterion::FileContent {
                path: "nope.txt".into(),
                equals: None,
                contains: Some("x".into()),
                regex: None,
            },
            &sys
        )
        .await
        .unwrap());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn grade_command_uses_exit_code() {
        let (dir, sys) = temp_system();
        assert!(grade(
            &Criterion::Command {
                run: "true".into(),
                expect_exit: 0,
                stdout_equals: None,
                stdout_contains: None,
                stdout_regex: None,
            },
            &sys
        )
        .await
        .unwrap());
        assert!(!grade(
            &Criterion::Command {
                run: "false".into(),
                expect_exit: 0,
                stdout_equals: None,
                stdout_contains: None,
                stdout_regex: None,
            },
            &sys
        )
        .await
        .unwrap());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn grade_command_matches_stdout() {
        let (dir, sys) = temp_system();
        // exit 0 AND stdout (trimmed) equals "42" → pass.
        assert!(grade(
            &Criterion::Command {
                run: "echo 42".into(),
                expect_exit: 0,
                stdout_equals: Some("42".into()),
                stdout_contains: None,
                stdout_regex: None,
            },
            &sys
        )
        .await
        .unwrap());
        // right exit but wrong stdout → fail (this is what catches a wrong-answer program).
        assert!(!grade(
            &Criterion::Command {
                run: "echo 41".into(),
                expect_exit: 0,
                stdout_equals: Some("42".into()),
                stdout_contains: None,
                stdout_regex: None,
            },
            &sys
        )
        .await
        .unwrap());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn grade_all_requires_every_subcriterion() {
        let (dir, sys) = temp_system();
        sys.write_file("a.txt", "hello").await.unwrap();
        let pass = Criterion::All {
            of: vec![
                Criterion::Command {
                    run: "true".into(),
                    expect_exit: 0,
                    stdout_equals: None,
                    stdout_contains: None,
                    stdout_regex: None,
                },
                Criterion::FileContent {
                    path: "a.txt".into(),
                    equals: None,
                    contains: Some("hell".into()),
                    regex: None,
                },
            ],
        };
        assert!(grade(&pass, &sys).await.unwrap());

        let fail = Criterion::All {
            of: vec![
                Criterion::Command {
                    run: "false".into(),
                    expect_exit: 0,
                    stdout_equals: None,
                    stdout_contains: None,
                    stdout_regex: None,
                },
                Criterion::FileContent {
                    path: "a.txt".into(),
                    equals: None,
                    contains: Some("hell".into()),
                    regex: None,
                },
            ],
        };
        assert!(!grade(&fail, &sys).await.unwrap());
        std::fs::remove_dir_all(&dir).ok();
    }
}
