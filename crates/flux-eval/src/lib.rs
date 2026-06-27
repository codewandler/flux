//! `flux-eval` — flux's eval & self-improvement harness.
//!
//! This crate is the substrate behind the improve flows (`improve-tbench.flux`): it runs flux against benchmark tasks, mines
//! the resulting sessions for pain-points, and scores the results — and it exposes those capabilities as
//! Flux-Lang **ops** (`flux_runtime::Tool`s) so the self-improvement workflow can be authored as a
//! Flux-Lang graph rather than a bespoke Rust program.
//!
//! Layering: this is an L3 crate. It depends only on L0–L2 (`flux-core`, `flux-spec`, `flux-runtime`,
//! `flux-system`, `flux-events`) and runs the flux binary **as a subprocess** — the only honest way
//! to measure the *rebuilt* binary inside the improvement loop. Model-driven steps (review, derive,
//! implement) are not ops here; the flow expresses them with the existing `task` sub-agent op.
//!
//! ## Modules
//! - [`spec`] — the normalized benchmark task format ([`spec::TaskSpec`] / [`spec::Criterion`]).
//! - [`adapter`] — the [`adapter::BenchmarkAdapter`] trait the runner drives.
//! - [`adapters`] — concrete adapters (offline `mock` fixture + `terminal-bench`; SWE-bench Lite later).
//! - [`metrics`] — [`metrics::RunResult`] + post-hoc extraction from a run's session store.
//! - [`score`] — [`score::SuiteScore`] + the lexicographic `is_better` comparison.
//! - [`runner`] — run one local task (materialize workspace → run flux → grade criterion).
//! - [`painpoint`] — deterministic pain-point mining over a session's message log.
//! - [`ops`] — the Flux-Lang `Tool` wrappers the improve flows call.

pub mod adapter;
pub mod adapters;
pub mod aggregate;
pub mod gate;
pub mod git;
pub mod metrics;
pub mod ops;
pub mod painpoint;
pub mod runner;
pub mod score;
pub mod spec;
pub mod transcript;
pub mod util;

use std::sync::Arc;

use flux_runtime::ToolRegistry;

/// Register the eval / self-improvement ops onto a tool registry (mirrors
/// [`flux_tools::register_builtins`](https://docs.rs/flux-tools)).
///
/// Wire this on the **top-level** registry only — these ops orchestrate eval runs and (later) mutate
/// git, so they belong to the outer flow, never to a worker sub-agent's scoped toolset.
pub fn register_eval_ops(registry: &mut ToolRegistry) {
    // Eval substrate.
    registry.register(Arc::new(ops::EvalRunTool));
    registry.register(Arc::new(ops::EvalSessionsTool));
    registry.register(Arc::new(ops::SessionsDigestTool));
    registry.register(Arc::new(ops::PainpointsCollectTool));
    registry.register(Arc::new(ops::ImproveLogTool));
    registry.register(Arc::new(ops::EvalAdoptTool));
    registry.register(Arc::new(ops::EvalScalarTool));
    registry.register(Arc::new(ops::ScoreCompareTool));
    registry.register(Arc::new(ops::GradeTool));
    // Aggregate → candidates + loop control.
    registry.register(Arc::new(aggregate::ImprovementsAggregateTool));
    registry.register(Arc::new(aggregate::CandidatesEmptyTool));
    registry.register(Arc::new(aggregate::CandidatesAdvanceTool));
    registry.register(Arc::new(ops::ChangeImplementTool));
    // Keep/commit/revert loop. `git_commit`/`git_stage` are built-ins (flux-tools); we add only what
    // they lack: a HEAD+clean snapshot, tagging, and a hard-reset revert.
    registry.register(Arc::new(gate::GateCheckTool));
    registry.register(Arc::new(git::GitSnapshotTool));
    registry.register(Arc::new(git::GitTagTool));
    registry.register(Arc::new(git::GitRevertTool));
    // Integrity: restore grader/harness/CI after the worker runs (anti-gaming).
    registry.register(Arc::new(git::GuardProtectedTool));
}

/// The evidence-gated [`ToolGroup`](flux_evidence::ToolGroup) bundling every eval / self-improvement
/// op. These are niche — relevant only when an eval workspace is present — so they are advertised to
/// the model only once an `eval` signal is observed (a `.flux/evals/` directory). Membership is read
/// back from [`register_eval_ops`] so the group can never drift from the registered ops.
pub fn eval_group() -> flux_evidence::ToolGroup {
    let mut reg = ToolRegistry::new();
    register_eval_ops(&mut reg);
    flux_evidence::ToolGroup {
        name: "eval".into(),
        description: "Evaluation & self-improvement operations (improve-tbench.flux).".into(),
        tools: reg.names(),
        surface_when: vec![flux_evidence::SignalMatch {
            kind: flux_evidence::KIND_SIGNAL.into(),
            signal: Some("eval".into()),
        }],
    }
}
