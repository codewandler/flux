//! [`Deliverer`] — the seam a channel calls to wake the program — and its production [`AppDeliverer`].

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::Mutex;

use flux_app::{App, JourneyRun};

/// What a channel adapter calls to wake the program on an event. Returns the journeys the event's
/// triggers ran (used for a synchronous reply). A test double can implement this without an [`App`].
#[async_trait]
pub trait Deliverer: Send + Sync {
    async fn deliver(&self, label: &str, payload: Value) -> anyhow::Result<Vec<JourneyRun>>;
}

/// The production deliverer: routes an event into a running [`App`]'s bus → triggers → journeys.
///
/// Deliveries are **serialized** by `gate`: [`App::deliver`] subscribes to the broadcast bus and drains
/// the cascade events its journeys emit, so two concurrent deliveries would each also receive the
/// other's cascade events (broadcast fan-out) and double-process them. One in-flight delivery at a time
/// avoids that. Journeys themselves run on independent per-run stores, so this is the only serialization
/// point; cross-channel concurrency would need per-delivery bus isolation (a follow-up).
pub struct AppDeliverer {
    app: Arc<App>,
    gate: Arc<Mutex<()>>,
}

impl AppDeliverer {
    pub fn new(app: Arc<App>) -> Self {
        Self {
            app,
            gate: Arc::new(Mutex::new(())),
        }
    }
}

#[async_trait]
impl Deliverer for AppDeliverer {
    async fn deliver(&self, label: &str, payload: Value) -> anyhow::Result<Vec<JourneyRun>> {
        let _guard = self.gate.lock().await;
        self.app
            .deliver(label.to_string(), payload)
            .await
            .map_err(|e| anyhow::anyhow!("deliver `{label}`: {e}"))
    }
}
