//! `flux-eval` — flux's eval & self-improvement harness.
//!
//! This crate is the substrate behind `improve.flux`: it runs flux against benchmark tasks, mines
//! the resulting sessions for pain-points, and scores suites — and it exposes those capabilities as
//! Flux-Lang **ops** (`flux_runtime::Tool`s) so the self-improvement workflow can be authored as a
//! Flux-Lang graph rather than a bespoke Rust program.
//!
//! Layering: this is an L3 crate. It depends only on L0–L2 (`flux-core`, `flux-spec`, `flux-runtime`,
//! `flux-system`, `flux-session`) and runs the flux binary **as a subprocess** — the only honest way
//! to measure the *rebuilt* binary inside the improvement loop. Model-driven steps (review, derive,
//! implement) are not ops here; the flow expresses them with the existing `task` sub-agent op.
//!
//! ## Modules
//! - [`spec`] — the normalized benchmark task format ([`spec::TaskSpec`] / [`spec::Criterion`]).
//! - [`adapter`] — the [`adapter::BenchmarkAdapter`] trait the runner drives.
//! - [`adapters`] — concrete adapters (`local`/`mock` now; terminal-bench + SWE-bench Lite at M5).
//! - [`metrics`] — [`metrics::RunResult`] + post-hoc extraction from a run's session store.
//! - [`score`] — [`score::SuiteScore`] + the lexicographic `is_better` comparison.
//! - [`runner`] — run one local task (materialize workspace → run flux → grade criterion).
//! - [`painpoint`] — deterministic pain-point mining over a session's message log.
//! - [`ops`] — the Flux-Lang `Tool` wrappers `improve.flux` calls.

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
    registry.register(Arc::new(ops::PainpointsCollectTool));
    registry.register(Arc::new(ops::EvalAdoptTool));
    registry.register(Arc::new(ops::EvalScalarTool));
    registry.register(Arc::new(ops::ScoreCompareTool));
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
}
