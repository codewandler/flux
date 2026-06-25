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
//! See `docs/designs/flux-flow.md` for the full design. Modules: the pure [`ast`] contracts, the
//! [`registry`] adapter over the existing tool registry, flux-flow's own [`state`] store (values,
//! symbols, run-event trace), the [`analyze`] validator, the [`compile`] front-end (natural language
//! → AST), the [`render`] pretty-printer, the [`runtime`] interpreter, and the [`engine`] turn loop.

pub mod analyze;
pub mod ast;
pub mod compile;
mod effects;
pub mod engine;
mod error;
pub mod registry;
pub mod render;
pub mod runtime;
pub mod state;

pub use error::{FlowError, Result};
