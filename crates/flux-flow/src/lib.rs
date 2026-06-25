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
//! See `docs/designs/flux-flow.md` for the full design. This is M0: the pure [`ast`] contracts and
//! the [`registry`] adapter over the existing tool registry.

pub mod ast;
mod effects;
mod error;
pub mod registry;

pub use error::{FlowError, Result};
