//! `flux-flow` — Flux-Lang: the LLM plans, the runtime runs.
//!
//! flux-flow is a deterministic execution engine. Instead of the model acting as the runtime
//! scheduler — deciding each step live and re-reading every tool output — the model is a compiler
//! *front-end* that turns an instruction into a typed, readable **execution graph** (an AST). A Rust
//! pipeline (`compile → analyze → optimize → execute`) resolves *symbols* to stored immutable
//! *values* and runs registered *operations* through the existing
//! [`Executor::dispatch`](flux_runtime) envelope, under policy, with risk-gated approval — and the
//! graph can be re-run later with the fewest possible model calls.
//!
//! This crate is L3: it depends on the runtime (L2) and a provider (L1) but reuses the safety
//! envelope rather than replacing it. Every operation lowers to a [`flux_spec::ToolSpec`] and runs
//! through `Executor::dispatch`, so there is no new bypass surface.
//!
//! The pure **language** half — the AST, renderer, analyzer, effect/op contracts, and the
//! schema/skill single source of truth — lives in the L0 [`flux_lang`] crate and is re-exported here
//! as a facade, so `flux_flow::{ast, render, analyze, …}` keep resolving. This crate owns only the
//! **engine**: the [`compile`] front-end (natural language → AST), the [`registry`] adapter over the
//! real tool registry, the [`runtime`] interpreter, the [`engine`] turn loop, and the [`state`] store.

pub mod agent_sink;
pub mod compile;
pub mod engine;
pub mod loop_host;
pub mod registry;
pub mod runtime;
pub mod state;
pub mod voice;

pub use agent_sink::AgentSink;
pub use voice::{tool_defs_from_registry, VoiceSessionDriver, VoiceSink, VoiceTurnHandler};

// Facade: the language core + reference interpreter live in `flux-lang`. Re-export them so the
// language surface stays available from the engine crate (no consumer churn) and
// `crate::{ast,render,analyze,host,store,…}` resolve inside the engine modules.
pub use flux_lang::{
    analyze, ast, effects, error, host, opspec, optimize, prelude, program, render, schema, sink,
    store,
};
pub use flux_lang::{FlowError, Result};
