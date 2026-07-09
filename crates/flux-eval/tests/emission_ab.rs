//! L-20/L-71: the planner-emission A/B — strict-JSON `ast` (control) vs native-text `source`
//! (treatment) vs merged one-node-object `ast` schema (L-71) — over the fixed corpus in
//! `assets/emission-ab/tasks.json`.
//!
//! Two tests:
//! - [`corpus_is_valid`] (hermetic, always runs): the committed fixture parses, ids are unique,
//!   and the corpus stays in the story's 10–20 task band.
//! - [`emission_ab_live`] (env-gated + `#[ignore]`, the flux-eval synthetic-loop precedent): runs
//!   EVERY arm over the corpus against a real model and prints the per-arm comparison table that
//!   `docs/designs/flux-lang-emission-ab.md` records. It spends real tokens, so it is skipped by
//!   default; run it with
//!
//!   ```text
//!   FLUX_EMISSION_AB=1 cargo test -p flux-eval --test emission_ab -- --ignored --nocapture
//!   ```
//!
//!   Requirements: `OPENROUTER_API_KEY` (the OpenRouter Anthropic-Messages wire), and optionally
//!   `FLUX_EMISSION_AB_MODEL` (default `anthropic/claude-sonnet-4.6` — the working default from
//!   the L-20 story). Metrics per arm: first-emission acceptance, repair rounds, tokens
//!   (uncached-in / cache / out, straight from the planner's [`flux_core::Usage`]), and wall time.

use std::time::Instant;

use serde::Deserialize;

use flux_core::pricing::PricingTable;
use flux_core::{Message, Usage};
use flux_flow::compile::{compile_turn_with_arm, CompileOptions, EmissionArm, Phase, TurnOutput};
use flux_flow::registry::OpRegistry;
use flux_runtime::ToolRegistry;

const CORPUS: &str = include_str!("../assets/emission-ab/tasks.json");

/// One fixed planning task: a natural-language prompt the planner must turn into a plan.
#[derive(Deserialize)]
struct Task {
    id: String,
    prompt: String,
    /// The node kinds / constructs the task is meant to exercise (documentation, not asserted —
    /// the planner is free to spell a correct plan differently).
    #[serde(default)]
    #[allow(dead_code)]
    exercises: Vec<String>,
}

fn corpus() -> Vec<Task> {
    serde_json::from_str(CORPUS).expect("emission-ab corpus parses")
}

/// Hermetic: the committed corpus is well-formed and sized per the story (10–20 tasks).
#[test]
fn corpus_is_valid() {
    let tasks = corpus();
    assert!(
        (10..=20).contains(&tasks.len()),
        "corpus must hold 10–20 tasks, has {}",
        tasks.len()
    );
    let mut ids = std::collections::HashSet::new();
    for t in &tasks {
        assert!(!t.prompt.trim().is_empty(), "task {} has a prompt", t.id);
        assert!(ids.insert(t.id.clone()), "duplicate task id {}", t.id);
    }
}

/// The outcome of one task under one arm.
struct TaskResult {
    id: String,
    /// `Some(attempts)` when a plan was accepted (attempts = the step it was accepted on, so
    /// `1` = first-emission acceptance and `attempts - 1` = repair rounds).
    accepted: Option<u32>,
    /// The model answered in prose instead of emitting a plan.
    chat: bool,
    /// Turn-level failure (step budget exhausted, truncation, provider error).
    error: Option<String>,
    usage: Usage,
    ms: u128,
}

struct ArmReport {
    results: Vec<TaskResult>,
}

impl ArmReport {
    fn accepted(&self) -> usize {
        self.results.iter().filter(|r| r.accepted.is_some()).count()
    }
    fn first_try(&self) -> usize {
        self.results
            .iter()
            .filter(|r| r.accepted == Some(1))
            .count()
    }
    fn repair_rounds(&self) -> u32 {
        self.results
            .iter()
            .filter_map(|r| r.accepted.map(|a| a - 1))
            .sum()
    }
    /// Field-wise sum across tasks. NOT [`Usage::accumulate`] — that folds calls *within* one
    /// turn (input side replaced, last call wins); across independent tasks every prompt is billed,
    /// so totals must add. (Within a repaired task the input side still reflects the final call —
    /// a small undercount, identical in both arms, noted in the design doc.)
    fn usage_total(&self) -> Usage {
        let mut u = Usage::default();
        for r in &self.results {
            u.input_tokens += r.usage.input_tokens;
            u.output_tokens += r.usage.output_tokens;
            u.cache_creation_input_tokens += r.usage.cache_creation_input_tokens;
            u.cache_read_input_tokens += r.usage.cache_read_input_tokens;
            u.reasoning_tokens += r.usage.reasoning_tokens;
        }
        u
    }
    fn wall_ms(&self) -> u128 {
        self.results.iter().map(|r| r.ms).sum()
    }
}

async fn run_arm(
    provider: &dyn flux_provider::Provider,
    model: &str,
    ops: &OpRegistry<'_>,
    tasks: &[Task],
    arm: EmissionArm,
) -> ArmReport {
    let mut results = Vec::new();
    for task in tasks {
        let opts = CompileOptions {
            // Bounded spend: up to 3 repair rounds per task, then the turn is rejected.
            max_steps: 4,
            ..Default::default()
        };
        let conversation = [Message::user_text(&task.prompt)];
        let started = Instant::now();
        // C-31 shape: usage arrives alongside the outcome, so a FAILED task now reports what it
        // actually spent instead of zeros.
        let (out, usage) = compile_turn_with_arm(
            provider,
            model,
            &conversation,
            None,
            ops,
            None,
            None,
            None,
            opts,
            arm,
            Phase::Execute,
        )
        .await;
        let ms = started.elapsed().as_millis();
        let r = match out {
            Ok(TurnOutput::Plan(c)) => TaskResult {
                id: task.id.clone(),
                accepted: Some(c.attempts),
                chat: false,
                error: None,
                usage,
                ms,
            },
            Ok(TurnOutput::Chat(_)) => TaskResult {
                id: task.id.clone(),
                accepted: None,
                chat: true,
                error: None,
                usage,
                ms,
            },
            Err(e) => TaskResult {
                id: task.id.clone(),
                accepted: None,
                chat: false,
                error: Some(e.to_string()),
                usage,
                ms,
            },
        };
        let status = match (&r.accepted, r.chat, &r.error) {
            (Some(1), _, _) => "ok (first emission)".to_string(),
            (Some(n), _, _) => format!("ok after {} repair round(s)", n - 1),
            (None, true, _) => "CHAT (no plan)".to_string(),
            (None, _, Some(e)) => format!("FAILED: {e}"),
            _ => "FAILED".to_string(),
        };
        eprintln!(
            "[{:?}] {:<22} {:>7} ms  in={} cw={} cr={} out={}  {}",
            arm,
            r.id,
            r.ms,
            r.usage.input_tokens,
            r.usage.cache_creation_input_tokens,
            r.usage.cache_read_input_tokens,
            r.usage.output_tokens,
            status
        );
        results.push(r);
    }
    ArmReport { results }
}

fn print_comparison(arms: &[(&str, &ArmReport)], model_spec: &str) {
    let n = arms
        .first()
        .map(|(_, r)| r.results.len())
        .unwrap_or_default();
    let per = |v: u64| v as f64 / n as f64;
    let pricing = PricingTable::builtin();
    let cost = |u: &Usage| {
        pricing
            .cost(u, model_spec)
            .map(|m| format!("${:.4}", m.usd))
            .unwrap_or_else(|| "n/a".to_string())
    };
    // One markdown table, one column per arm — the L-71 third arm rides in the same table the
    // design doc records, so N arms print without another format.
    let row = |label: &str, cells: Vec<String>| eprintln!("| {label} | {} |", cells.join(" | "));
    eprintln!("\n## Emission A/B — {model_spec} ({n} tasks/arm)\n");
    eprintln!(
        "| metric | {} |",
        arms.iter()
            .map(|(l, _)| l.to_string())
            .collect::<Vec<_>>()
            .join(" | ")
    );
    eprintln!("|---|{}|", "---|".repeat(arms.len()));
    let cells = |f: &dyn Fn(&ArmReport) -> String| arms.iter().map(|(_, r)| f(r)).collect();
    row(
        "plans accepted",
        cells(&|r| format!("{}/{}", r.accepted(), r.results.len())),
    );
    row(
        "first-emission acceptance",
        cells(&|r| format!("{}/{}", r.first_try(), r.results.len())),
    );
    row(
        "repair rounds (total)",
        cells(&|r| r.repair_rounds().to_string()),
    );
    row(
        "uncached input tok (total / per task)",
        cells(&|r| {
            let u = r.usage_total();
            format!("{} / {:.0}", u.input_tokens, per(u.input_tokens))
        }),
    );
    row(
        "cache-write tok (total)",
        cells(&|r| r.usage_total().cache_creation_input_tokens.to_string()),
    );
    row(
        "cache-read tok (total)",
        cells(&|r| r.usage_total().cache_read_input_tokens.to_string()),
    );
    row(
        "output tok (total / per task)",
        cells(&|r| {
            let u = r.usage_total();
            format!("{} / {:.0}", u.output_tokens, per(u.output_tokens))
        }),
    );
    row(
        "wall time (total / per task)",
        cells(&|r| {
            format!(
                "{:.1}s / {:.1}s",
                r.wall_ms() as f64 / 1000.0,
                r.wall_ms() as f64 / 1000.0 / n as f64
            )
        }),
    );
    row("est. cost", cells(&|r| cost(&r.usage_total())));
}

#[tokio::test]
#[ignore = "live A/B — spends real tokens; FLUX_EMISSION_AB=1 + OPENROUTER_API_KEY, run with --ignored --nocapture"]
async fn emission_ab_live() {
    if std::env::var("FLUX_EMISSION_AB").ok().as_deref() != Some("1") {
        eprintln!("emission_ab_live: skipped (set FLUX_EMISSION_AB=1 to run — spends real tokens)");
        return;
    }
    let provider = match flux_providers::openrouter::openrouter_anthropic_from_env() {
        Ok(p) => p,
        Err(e) => {
            panic!("emission_ab_live: OPENROUTER_API_KEY must resolve for the live run: {e}");
        }
    };
    let model = std::env::var("FLUX_EMISSION_AB_MODEL")
        .unwrap_or_else(|_| "anthropic/claude-sonnet-4.6".to_string());
    let model_spec = format!("openrouter-anthropic/{model}");

    let mut reg = ToolRegistry::new();
    flux_tools::register_builtins(&mut reg);
    let ops = OpRegistry::new(&reg);
    let tasks = corpus();

    eprintln!(
        "emission A/B: {} tasks per arm, model {model_spec}\n",
        tasks.len()
    );
    let json = run_arm(&provider, &model, &ops, &tasks, EmissionArm::Json).await;
    let text = run_arm(&provider, &model, &ops, &tasks, EmissionArm::Text).await;
    let merged = run_arm(&provider, &model, &ops, &tasks, EmissionArm::Merged).await;
    print_comparison(
        &[
            ("json (ast schema)", &json),
            ("text (native source)", &text),
            ("merged (one-node schema)", &merged),
        ],
        &model_spec,
    );

    // The run itself must have been able to measure: every task produced *some* outcome. (Plan
    // quality/acceptance is the experiment's RESULT, recorded in the design doc — not asserted.)
    assert_eq!(json.results.len(), tasks.len());
    assert_eq!(text.results.len(), tasks.len());
    assert_eq!(merged.results.len(), tasks.len());
}
