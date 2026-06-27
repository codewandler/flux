//! `flux-lang` — the pure Flux-Lang language core.
//!
//! Flux-Lang is the planning language: the LLM emits a typed execution **graph** (an AST) and a
//! deterministic runtime runs it. This crate is the *language* half of that idea, deliberately
//! separated from the engine that compiles and executes it:
//!
//! - [`ast`] — the Draft AST the model emits, the typed HIR, the physical plan, the value model, the
//!   semantic [`ast::FlowEffect`]s, and the run-event trace.
//! - [`render`] — the AST pretty-printer (human-auditable projections).
//! - [`analyze`] — the validator, working against an abstract [`opspec::OpCatalog`] (no knowledge of
//!   any concrete tool registry).
//! - [`opspec`] — the typed operation spec/signature and the [`opspec::OpCatalog`] seam.
//! - [`effects`] — lowering of semantic effects onto host [`flux_spec::Effect`] + policy actions.
//! - [`schema`] — the single source of truth: a derived JSON Schema and the node-kind catalog that
//!   drives the planner prompt and the generated skill/docs.
//!
//! It is an **L0 leaf**: it depends only on other pure contracts (`flux-core`, `flux-spec`,
//! `flux-policy`) and has no IO, no provider, no runtime, and no dependency on concrete tools. The
//! engine crate `flux-flow` builds on top of it (compile → analyze → execute) and re-exports it.

pub mod analyze;
pub mod ast;
pub mod effects;
pub mod error;
pub mod host;
pub mod opspec;
pub mod render;
pub mod runtime;
pub mod schema;
pub mod sink;
pub mod skill;
pub mod store;

pub use error::{FlowError, Result};
