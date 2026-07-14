//! `flux-flow` — typed model judgment inside authored deterministic control flow.
//!
//! flux-flow is the agent engine around Flux-Lang. An authored outer loop invokes typed model
//! stages, resolves their capability signals against the live registry, gathers through exact
//! provider-native operation schemas, and freezes proposed effects into an approved action batch.
//! Models never author executable Flux. The deterministic language pipeline (`parse → analyze →
//! optimize → execute`) resolves *symbols* to stored immutable *values* and runs registered
//! *operations* through the existing
//! [`Executor::dispatch`](flux_runtime) envelope, under policy, with risk-gated approval — and the
//! authored graph can be replayed or resumed without turning model text into a runtime.
//!
//! This crate is L3: it depends on the runtime (L2) and a provider (L1) but reuses the safety
//! envelope rather than replacing it. Every operation lowers to a [`flux_spec::ToolSpec`] and runs
//! through `Executor::dispatch`, so there is no new bypass surface.
//!
//! The pure **language** half — the AST, renderer, analyzer, effect/op contracts, and the
//! schema/skill single source of truth — lives in the L0 [`flux_lang`] crate and is re-exported here
//! as a facade, so `flux_flow::{ast, render, analyze, …}` keep resolving. This crate owns only the
//! **engine**: the typed adaptive stages, the [`registry`] adapter over the real tool registry, the
//! [`runtime`] interpreter, the [`engine`] turn loop, and the [`state`] store.

pub mod agent_sink;
pub mod cassette;
pub mod composites;
pub mod engine;
pub mod fork;
pub mod loop_host;
mod model;
pub mod registry;
pub mod replay;
pub mod runtime;
mod staged;
pub use staged::{
    statically_gather_safe, AdaptiveLoopPolicy, AgentStagePolicy, ModelStageDefinition,
    DEFAULT_ADAPTIVE_MODEL_CALLS,
};
pub mod state;
pub mod voice;

pub use agent_sink::AgentSink;
pub use engine::{DEFAULT_AGENT_LOOP_ITERATIONS, MAX_AGENT_LOOP_ITERATIONS};
pub use voice::{
    tool_defs_from_registry, TranscriptAccumulator, UsageRecording, VoiceSessionDriver, VoiceSink,
    VoiceTurnHandler,
};

// Facade: the language core + reference interpreter live in `flux-lang`. Re-export them so the
// language surface stays available from the engine crate (no consumer churn) and
// `crate::{ast,render,analyze,host,store,…}` resolve inside the engine modules.
pub use flux_lang::{
    analyze, ast, context_slice, effects, error, host, opspec, optimize, prelude, program, render,
    schema, sink, store,
};
pub use flux_lang::{FlowError, Result};
