//! The crate error type. Library code returns [`Result`]; the phases (`parse`/`analyze`/`compile`/
//! `runtime`) each map onto a variant so diagnostics stay attributable.

/// The result alias for flux-flow.
pub type Result<T> = std::result::Result<T, FlowError>;

/// A failure in one of the Flux-Lang pipeline phases.
#[derive(Debug, thiserror::Error)]
pub enum FlowError {
    /// The compact syntax or JSON AST could not be parsed.
    #[error("parse error: {0}")]
    Parse(String),

    /// The analyzer rejected the AST (unknown symbol/op, type mismatch, forbidden effect, …).
    #[error("analyze error: {0}")]
    Analyze(String),

    /// The LLM front-end failed to produce a valid AST (after repair attempts).
    #[error("compile error: {0}")]
    Compile(String),

    /// A failure while executing a plan.
    #[error("runtime error: {0}")]
    Runtime(String),

    /// An error bubbled up from a lower flux layer.
    #[error(transparent)]
    Core(#[from] flux_core::Error),
}
