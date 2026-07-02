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

    /// The analyzer rejected the AST (unknown op, unbound symbol, illegal expression position,
    /// invalid name, arity/required-param or scalar type mismatch, unbounded loop, …). Effect
    /// *authorization* is not the analyzer's job — policy decides allow/deny/approve at dispatch.
    #[error("analyze error: {0}")]
    Analyze(String),

    /// The LLM front-end failed to produce a valid AST (after repair attempts).
    #[error("compile error: {0}")]
    Compile(String),

    /// A failure while executing a plan.
    #[error("runtime error: {0}")]
    Runtime(String),

    /// The host's safety envelope **refused to run** an op — a policy / permission-rule /
    /// capability-scope / user-approval denial, marked by the host on the dispatch outcome
    /// ([`OpOutcome::denied`](crate::host::OpOutcome)). Structured (not the in-band error string a
    /// failed op surfaces as) so [`is_fatal`](Self::is_fatal) can see it: re-attempting a
    /// deliberate refusal can never succeed and only re-prompts/re-audits the same denial (L-21).
    #[error("op denied: {0}")]
    Denied(String),

    /// An error bubbled up from a lower flux layer.
    #[error(transparent)]
    Core(#[from] flux_core::Error),
}

impl FlowError {
    /// Whether this error is **fatal** — a deliberate refusal (a denied `confirm`, an op
    /// [`Denied`](Self::Denied) by the host's safety envelope) or a failed invariant
    /// (`AssertFailed`) — rather than a transient failure. Fatal errors must never be re-attempted
    /// or silently absorbed by the reliability wrappers: `retry` propagates them immediately, and
    /// the wrap points (`loop`'s body re-wrap, the composite-op boundary) preserve their structural
    /// identity instead of stringifying them, so an enclosing `retry` still sees them.
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            FlowError::Denied(_)
                | FlowError::Core(flux_core::Error::AssertFailed(_))
                | FlowError::Core(flux_core::Error::ConfirmDenied(_))
        )
    }
}
