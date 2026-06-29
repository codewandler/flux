//! `flux-markdown` — frontmatter parsing/validation plus (feature-gated) markdown rendering.
//!
//! The **frontmatter** half is pure and lives at L0 so `flux-skill` and `flux-orchestrate` can share
//! one `---`-delimited parser (driven by a real YAML backend, [`serde_norway`]) instead of each
//! hand-rolling a lenient flat parser. You describe a frontmatter *format* with a serde struct and
//! [`parse_frontmatter`] fills it — the type *is* the schema.
//!
//! The **rendering** half is a thin wrapper over the `codewandler/markdown` crates, behind
//! off-by-default cargo features (`ratatui`, `terminal`) so the default build — and the L0 layering —
//! stays free of heavy UI dependencies. See [`render`].

pub mod frontmatter;

pub use frontmatter::{
    compose_frontmatter, parse_frontmatter, render_document, split_frontmatter, Document,
};

#[cfg(any(feature = "ratatui", feature = "terminal"))]
pub mod render;
