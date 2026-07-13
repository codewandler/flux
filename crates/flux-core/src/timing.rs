use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Attribution for one operation dispatch, measured at the safety-envelope boundary.
///
/// Microseconds preserve short local-tool timings while remaining portable, serializable integers.
/// `None` means the phase did not occur: a pre-authorized call has no approval wait, and a denied
/// call has no execution duration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationTiming {
    pub total_us: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_wait_us: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_us: Option<u64>,
}

impl OperationTiming {
    pub fn from_durations(
        total: Duration,
        approval_wait: Option<Duration>,
        execution: Option<Duration>,
    ) -> Self {
        Self {
            total_us: duration_us(total),
            approval_wait_us: approval_wait.map(duration_us),
            execution_us: execution.map(duration_us),
        }
    }
}

fn duration_us(duration: Duration) -> u64 {
    duration.as_micros().min(u64::MAX as u128) as u64
}
