//! Run one local benchmark task: materialize a workspace, drive flux headlessly in it, grade the
//! success criterion **outside** the agent, and recover metrics from the child's isolated session log.
//!
//! Isolation: each task gets a fresh temp workspace (the agent's cwd) and a private `HOME`
//! (`<workdir>/.home`) so the child's `~/.flux/sessions.db` never collides with the parent's or with
//! other tasks. The criterion is graded through a [`System`] rooted at the workspace — argv-only, no
//! shell — so the agent can't "pass" by tampering with its own grader.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use regex::Regex;

use flux_core::{Error, Message, Result, Usage};
use flux_events::EventStore;
use flux_system::{System, Workspace};

use flux_flow::ast::RunEvent;

use crate::adapter::RunContext;
use crate::metrics::{iterations_from_messages, metrics_from_events, RunResult};
use crate::spec::{Criterion, SeedFile, Setup, TaskSpec};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn io_err(e: std::io::Error) -> Error {
    Error::Other(e.to_string())
}

/// A unique temp directory (created) under the system temp dir.
fn unique_temp_dir(prefix: &str) -> Result<PathBuf> {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("{prefix}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(io_err)?;
    Ok(dir)
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
        std::fs::create_dir_all(parent).map_err(io_err)?;
    }
    std::fs::write(&dest, &f.content).map_err(io_err)
}

fn copy_dir_recursive(from: &Path, to: &Path) -> Result<()> {
    for entry in std::fs::read_dir(from).map_err(io_err)? {
        let entry = entry.map_err(io_err)?;
        let src = entry.path();
        let dest = to.join(entry.file_name());
        if src.is_dir() {
            std::fs::create_dir_all(&dest).map_err(io_err)?;
            copy_dir_recursive(&src, &dest)?;
        } else {
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

    let workdir = unique_temp_dir("flux-eval-task")?;
    materialize(&spec.setup, &workdir)?;
    let home = workdir.join(".home");
    std::fs::create_dir_all(&home).map_err(io_err)?;

    let model = spec
        .model
        .clone()
        .unwrap_or_else(|| ctx.default_model.to_string());

    let sys = System::new(
        Workspace::new(&workdir)
            .map_err(|e| Error::Other(format!("eval workspace {}: {e}", workdir.display())))?,
    );

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
    // Forward provider credentials to the eval child. flux-system scrubs the env and we isolate HOME,
    // so without this the child can't authenticate any real model. The child IS flux running a task —
    // the harness trusts itself; the child's own bash/process tools still scrub their subprocess env,
    // and the child's output is captured by the harness (never shown to a model).
    for key in [
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "OPENROUTER_API_KEY",
        "FLUX_SECRET",
    ] {
        if let Ok(val) = std::env::var(key) {
            env.push((key.to_string(), val));
        }
    }
    // Rust toolchain (so the child's own `cargo`/`rustup` tools work under the isolated HOME).
    env.extend(toolchain_env());
    for (k, v) in &spec.env {
        env.push((k.clone(), v.clone()));
    }
    // In watch mode, reveal the loop machinery so the observer sees plan/run_plan/observe.
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

    let passed = if timed_out {
        false
    } else {
        match grade(&spec.criterion, &sys).await {
            Ok(p) => p,
            Err(e) => {
                if note.is_none() {
                    note = Some(format!("grade error: {e}"));
                }
                false
            }
        }
    };

    Ok(RunResult {
        task_id: spec.id.clone(),
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

    fn temp_system() -> (PathBuf, System) {
        let dir = unique_temp_dir("flux-eval-runner-test").unwrap();
        let sys = System::new(Workspace::new(&dir).unwrap());
        (dir, sys)
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
