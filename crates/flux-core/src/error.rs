//! The shared error type.
//!
//! Kept dependency-light on purpose: provider crates map their transport errors (e.g. reqwest)
//! into these variants rather than this crate depending on them.

/// The crate-wide result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Why an operation behind the guarded-IO port did not produce a value.
///
/// This lives in the shared error contract because the distinction must survive type erasure and
/// delegation. Recovering it from formatted text would let a caller-controlled path or a
/// delegate-authored refusal reason impersonate a broken transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardedIoFailure {
    /// The substrate answered and refused the operation.
    Refused,
    /// No answer arrived from the delegated substrate.
    Unreachable,
    /// The substrate accepted the operation but cannot prove its terminal outcome.
    Unknown,
    /// The substrate does not implement the operation.
    Unserved,
}

impl GuardedIoFailure {
    /// The stable operator-facing prefix for this failure kind.
    ///
    /// Classification never reads this text; it is public only so diagnostics and tests can quote
    /// the canonical spelling without copying it.
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::Refused => "the remote guarded substrate refused: ",
            Self::Unreachable => "the remote guarded delegate is unreachable: ",
            Self::Unknown => "the remote guarded operation has an unknown outcome: ",
            Self::Unserved => "this guarded substrate cannot ",
        }
    }
}

/// A structurally classified guarded-IO failure with an operator-facing detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardedIoError {
    kind: GuardedIoFailure,
    detail: String,
}

impl GuardedIoError {
    /// Construct a guarded-IO failure. `detail` never participates in classification.
    pub fn new(kind: GuardedIoFailure, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }

    /// The structural failure kind.
    pub fn kind(&self) -> GuardedIoFailure {
        self.kind
    }

    /// The unprefixed diagnostic supplied by the substrate or transport.
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl std::fmt::Display for GuardedIoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}{}", self.kind.prefix(), self.detail)
    }
}

impl std::error::Error for GuardedIoError {}

/// The shared flux error type.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A provider-side failure (transport, stream, or protocol).
    #[error("provider error: {0}")]
    Provider(String),

    /// Provider/model-originated bytes failed to decode mid-stream. Distinct from `Provider`
    /// (transport/protocol) so consumers can retry the call rather than kill the turn.
    #[error("provider stream decode error: {0}")]
    StreamDecode(String),

    /// A non-success HTTP response from a provider API.
    #[error("api error (status {status}): {message}")]
    Api { status: u16, message: String },

    /// An HTTP/transport error.
    #[error("http error: {0}")]
    Http(String),

    /// (De)serialization failure.
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),

    /// Local IO failure.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// Invalid or missing configuration.
    #[error("config error: {0}")]
    Config(String),

    /// Authentication/credentials failure.
    #[error("auth error: {0}")]
    Auth(String),

    /// Anything else.
    #[error("{0}")]
    Other(String),

    /// A guarded-IO operation was refused, unreachable, or not served.
    #[error(transparent)]
    GuardedIo(#[from] GuardedIoError),

    /// An assertion node failed its condition.
    #[error("assertion failed: {0}")]
    AssertFailed(String),

    /// A `confirm` node was denied by the approver.
    #[error("confirm denied: {0}")]
    ConfirmDenied(String),
}
