//! The [`Channel`] trait — a long-running external event source.

use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::Deliverer;

/// A long-running event source: a cron schedule, a webhook server, a Slack socket. Each implementation
/// owns its protocol loop and, per external event, calls `d.deliver(self.name(), payload)` to wake the
/// program. The returned [`JourneyRun`](flux_app::JourneyRun)s are the journeys the event's triggers ran
/// — an adapter uses them for a synchronous reply (the webhook response, a Slack thread post) or ignores
/// them (cron is fire-and-forget).
#[async_trait]
pub trait Channel: Send + Sync {
    /// The channel name — also the **event label** it delivers under. Wire a `trigger { on = <name> }`.
    fn name(&self) -> &str;

    /// A registered tool this channel needs before it may run, if any — asserted by
    /// [`crate::serve`] **before any channel task is spawned**.
    ///
    /// Defaulted to `None` so every existing adapter is unaffected. It exists for the one class of
    /// refusal a decl-only builder cannot make: whether a *tool* exists is a question about the
    /// live [`App`](flux_app::App)'s registry, not about a declaration. A connector binding's reply
    /// operation is the case — a binding that names an operation this host cannot dispatch is a
    /// channel that accepts deliveries and then cannot answer them, and finding that out on the
    /// first delivery is finding it out too late.
    fn required_tool(&self) -> Option<&str> {
        None
    }

    /// Run the protocol loop until `cancel` fires. Returning `Ok(())` ends the channel normally; an
    /// `Err` is a fatal channel error that brings the host down.
    async fn start(&self, d: Arc<dyn Deliverer>, cancel: CancellationToken) -> anyhow::Result<()>;
}
