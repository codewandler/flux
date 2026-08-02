//! `flux-lang` — the pure Flux-Lang language core.
//!
//! Flux-Lang is an authored workflow language: applications place deterministic control flow around
//! typed model stages and registered operations, and a deterministic runtime runs the AST. This crate
//! is the language half, deliberately separated from the engine that executes it:
//!
//! - [`ast`] — the Draft AST, typed HIR, physical plan, value model, the
//!   semantic [`ast::FlowEffect`]s, and the run-event trace.
//! - [`render`] — the AST pretty-printer (human-auditable projections).
//! - [`format`] / [`parse`] — the canonical compact **text syntax** (the round-trippable `.flux`
//!   surface): `parse(&format(&ast)) == ast` for every `DraftAst`. Distinct from `render` (one-way).
//! - [`glyph`] — **Flux Glyph**, the compact indented opcode *projection* of the same AST, selected
//!   explicitly (never sniffed) and round-trippable in its own right.
//! - [`analyze`] — the validator, working against an abstract [`opspec::OpCatalog`] (no knowledge of
//!   any concrete tool registry).
//! - [`opspec`] — the typed operation spec/signature and the [`opspec::OpCatalog`] seam.
//! - [`prelude`] — the artifact-type ontology (claims, evidence, needs, context packs, …) ops declare
//!   their I/O against; a stdlib of `Named` schemas, not a `Value` change.
//! - [`program`] — the multi-agent `Program` layer (agents/channels/triggers/journeys) + the
//!   key-sniffing module loader; pure-data decls the L6 `flux-app` host runs.
//! - [`effects`] — lowering of semantic effects onto host [`flux_spec::Effect`] + policy actions.
//! - [`schema`] — the single source of truth: a derived JSON Schema and the node-kind catalog that
//!   drives generated skill/docs and language tooling.
//! - [`context_slice`] — automatic context slicing (KF4/L-56): derive the minimum model-visible
//!   context for a model-op call or diagnostic UI from HIR reads, field access paths, op
//!   schemas, and diagnostics, gated by visibility/secret/policy and a token budget.
//!
//! It is an **L0 leaf**: it depends only on other pure contracts (`flux-core`, `flux-spec`,
//! `flux-policy`) and has no IO, no provider, no runtime, and no dependency on concrete tools. The
//! engine crate `flux-flow` builds on top of it (analyze → execute) and re-exports it.

pub mod analyze;
pub mod ast;
pub mod canonicalize;
pub mod context_slice;
mod cst_decode;
pub mod dsl;
pub mod editor;
pub mod effects;
pub mod error;
pub mod expr;
pub mod format;
pub mod format_cst;
pub mod glyph;
pub mod highlight;
pub mod host;
pub mod lexer;
pub mod lower_cst;
pub mod opspec;
pub mod optimize;
pub mod parse;
pub mod parser;
pub mod prelude;
pub mod program;
/// Railflux — the 7-bit ASCII dataflow projection. Private: its entry points are re-exported from
/// [`render`], which is the one public home for AST projections.
mod rail;
pub mod render;
pub mod runtime;
pub mod schema;
pub mod sink;
pub mod skill;
pub mod store;
pub mod syntax;

pub use error::{FlowError, Result};
