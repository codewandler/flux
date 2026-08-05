use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// A process-unique identifier for one tool/op dispatch (C-531).
///
/// The durable event log pairs a call with its result by step id; the live sink stream had no such
/// pairing, so a surface could only match a result back to its call by operation name and arrival
/// order. That is unsound the moment two same-name calls overlap — C-528's
/// `flush_parallel_native_calls` admits concurrent idempotent reads, and they may complete in any
/// order. The interpreter mints one of these per dispatch and stamps it on the call, the timing,
/// and the result, so a surface pairs on identity instead of on a guess.
///
/// This is a correlation token, not a capability: it names nothing, authorizes nothing, and is safe
/// to render or serialize. It is unique within a process, not across processes or restarts.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[serde(transparent)]
pub struct DispatchId(u64);

static NEXT_DISPATCH_ID: AtomicU64 = AtomicU64::new(1);

impl DispatchId {
    /// Mint the next id. Called once per dispatch, at the point the call is surfaced.
    pub fn next() -> Self {
        Self(NEXT_DISPATCH_ID.fetch_add(1, Ordering::Relaxed))
    }

    /// Rebuild an id from its wire representation — for a client decoding a `stream-json`
    /// transcript, and for tests that need deterministic ids.
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    /// The wire representation.
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for DispatchId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minted_ids_are_distinct_and_serialize_as_plain_numbers() {
        let first = DispatchId::next();
        let second = DispatchId::next();
        assert_ne!(first, second);
        assert_eq!(
            serde_json::to_string(&first).unwrap(),
            first.get().to_string()
        );
        assert_eq!(
            serde_json::from_str::<DispatchId>("42").unwrap(),
            DispatchId::from_raw(42)
        );
    }
}
