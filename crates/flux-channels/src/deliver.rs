//! [`Deliverer`] — the seam a channel calls to wake the program — and its production [`AppDeliverer`].

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use flux_app::{App, JourneyRun};

/// What a channel adapter calls to wake the program on an event. Returns the journeys the event's
/// triggers ran (used for a synchronous reply). A test double can implement this without an [`App`].
#[async_trait]
pub trait Deliverer: Send + Sync {
    async fn deliver(&self, label: &str, payload: Value) -> anyhow::Result<Vec<JourneyRun>>;
}

/// The production deliverer: routes an event into a running [`App`]'s bus → triggers → journeys.
///
/// [`App::deliver`] owns delivery serialization because its broadcast subscription drains cascade
/// events. Keeping that invariant on the shared App also covers direct callers and independently
/// constructed adapters.
pub struct AppDeliverer {
    app: Arc<App>,
}

impl AppDeliverer {
    pub fn new(app: Arc<App>) -> Self {
        Self { app }
    }
}

#[async_trait]
impl Deliverer for AppDeliverer {
    async fn deliver(&self, label: &str, payload: Value) -> anyhow::Result<Vec<JourneyRun>> {
        self.app
            .deliver(label.to_string(), payload)
            .await
            .map_err(|e| anyhow::anyhow!("deliver `{label}`: {e}"))
    }
}
