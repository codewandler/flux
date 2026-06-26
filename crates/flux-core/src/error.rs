//! The shared error type.
//!
//! Kept dependency-light on purpose: provider crates map their transport errors (e.g. reqwest)
//! into these variants rather than this crate depending on them.

/// The crate-wide result alias.
pub type Result<T> = std::result::Result<T, Error>;

/// The shared flux error type.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A provider-side failure (transport, stream, or protocol).
    #[error("provider error: {0}")]
    Provider(String),

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

    /// An assertion node failed its condition.
    #[error("assertion failed: {0}")]
    AssertFailed(String),

    /// A `confirm` node was denied by the approver.
    #[error("confirm denied: {0}")]
    ConfirmDenied(String),
}
