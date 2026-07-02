//! Markdown rendering on flux's own engine (L-02): parse to the [`crate::ast`] tree, lay out via
//! one shared width-aware engine, and emit either ratatui spans (`ratatui` feature) or ANSI
//! (`terminal` feature). Formerly thin wrappers over the external `codewandler/markdown` crates —
//! those remain only as dev-dependency parity oracles for this module's tests.

pub(crate) mod layout;

#[cfg(feature = "ratatui")]
pub mod tui;
#[cfg(feature = "ratatui")]
pub use tui::render;

#[cfg(feature = "terminal")]
pub mod term;
#[cfg(feature = "terminal")]
pub use term::{render_ansi, LiveRenderer, Theme};
