//! `flux-markdown` — flux's own markdown engine: frontmatter parsing plus an AST-based
//! CommonMark-subset parser, writer, and renderers.
//!
//! The **frontmatter** half is pure and lives at L0 so `flux-skill` and `flux-orchestrate` can share
//! one `---`-delimited parser (driven by a real YAML backend, [`serde_norway`]) instead of each
//! hand-rolling a lenient flat parser. You describe a frontmatter *format* with a serde struct and
//! [`parse_frontmatter`] fills it — the type *is* the schema.
//!
//! The **markdown engine** (L-02) is goldmark-style and owned outright: [`parser`] builds the
//! [`ast`] block+inline tree (see the parser docs for the supported subset and the deliberate
//! omissions), [`writer`] emits canonical markdown back out (AST round-trip stable), and the
//! feature-gated [`render`] paths (`ratatui`, `terminal`) lay the same AST out through one shared
//! width-aware engine. Extend it by walking the public AST — a custom renderer is a function over
//! [`ast::Document`], not a parser fork.

pub mod ast;
mod inline;
pub mod parser;
pub mod writer;

pub mod frontmatter;

pub use frontmatter::{
    compose_frontmatter, parse_frontmatter, render_document, split_frontmatter, Document,
};

#[cfg(any(feature = "ratatui", feature = "terminal"))]
pub mod render;
