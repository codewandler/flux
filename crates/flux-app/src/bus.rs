//! The in-process event bus: a `tokio::sync::broadcast` channel carrying [`Event`]s, plus a shared
//! record of every message sent on a channel (so a host or a test can observe what a journey produced).
//!
//! "User input is just an event" — a channel read injects a `user_input` event, a clock injects a
//! `cron:*` event, a journey's `emit` op publishes an arbitrary label. Triggers ([`crate::App`]) map
//! a label back to a journey. The bus carries the labels; the supervisor does the routing.

use std::collections::VecDeque;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use crate::supervisor::DeliveryMessage;

/// The observation-channel depth. Generous so observers can absorb a burst of journey `emit`s.
pub(crate) const CAPACITY: usize = 1024;

tokio::task_local! {
    static ACTIVE_DELIVERY: DeliveryOrigin;
}

#[derive(Clone)]
pub(crate) struct DeliveryOrigin {
    pub(crate) supervisor: u64,
    pub(crate) cascades: Arc<Mutex<VecDeque<Event>>>,
}

impl DeliveryOrigin {
    fn push(&self, event: Event) -> bool {
        let mut cascades = self.cascades.lock().expect("delivery cascades poisoned");
        if cascades.len() >= CAPACITY {
            return false;
        }
        cascades.push_back(event);
        true
    }
}

#[derive(Clone)]
struct DeliveryRouter {
    supervisor: u64,
    sender: mpsc::WeakSender<DeliveryMessage>,
    external_run: Arc<Mutex<Option<RunContext>>>,
}

#[derive(Clone)]
pub(crate) struct RunContext {
    id: u64,
    cancellation: CancellationToken,
    accepting: Arc<AtomicBool>,
    errors: mpsc::Sender<String>,
}

impl RunContext {
    pub(crate) fn new() -> (Self, mpsc::Receiver<String>) {
        static NEXT_RUN_ID: AtomicU64 = AtomicU64::new(1);
        let (errors, receiver) = mpsc::channel(1);
        (
            Self {
                id: NEXT_RUN_ID.fetch_add(1, Ordering::Relaxed),
                cancellation: CancellationToken::new(),
                accepting: Arc::new(AtomicBool::new(false)),
                errors,
            },
            receiver,
        )
    }

    pub(crate) fn cancel(&self) {
        self.cancellation.cancel();
        self.accepting.store(false, Ordering::Release);
    }

    pub(crate) fn activate(&self) {
        self.accepting.store(true, Ordering::Release);
    }

    pub(crate) fn accepts(&self) -> bool {
        self.accepting.load(Ordering::Acquire) && !self.cancellation.is_cancelled()
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    pub(crate) fn same(&self, other: &Self) -> bool {
        self.id == other.id
    }

    pub(crate) async fn cancelled(&self) {
        self.cancellation.cancelled().await;
    }

    pub(crate) fn report(&self, error: String) {
        let _ = self.errors.try_send(error);
    }
}

/// One event on the bus: a string `label` (the trigger key) and an arbitrary JSON `payload`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub label: String,
    pub payload: serde_json::Value,
}

impl Event {
    pub fn new(label: impl Into<String>, payload: serde_json::Value) -> Self {
        Self {
            label: label.into(),
            payload,
        }
    }
}

/// A message a journey wrote to a named channel via the `send`/`ask` ops. Recorded so a host can
/// render it and tests can assert on it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SentMessage {
    pub channel: String,
    pub message: String,
    /// `true` when produced by `ask` (a message that expects a reply), `false` for plain `send`.
    pub expects_reply: bool,
}

/// A cloneable handle to the in-process event bus. Cloning shares the same underlying broadcast
/// channel and the same recorded-message log, so every op-pack instance and the supervisor see one bus.
#[derive(Clone)]
pub struct Bus {
    tx: broadcast::Sender<Event>,
    sent: Arc<Mutex<Vec<SentMessage>>>,
    delivery: Arc<Mutex<Option<DeliveryRouter>>>,
}

impl Bus {
    /// Create a fresh bus with no subscribers.
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(CAPACITY);
        Self {
            tx,
            sent: Arc::new(Mutex::new(Vec::new())),
            delivery: Arc::new(Mutex::new(None)),
        }
    }

    /// Publish an event. Returns the number of live observers plus the private App delivery route
    /// when it accepted the event (`0` when no one is listening is a valid fire-and-forget no-op).
    pub fn emit(&self, label: impl Into<String>, payload: serde_json::Value) -> usize {
        let event = Event::new(label, payload);
        let observers = self.tx.send(event.clone()).unwrap_or(0);
        let routed = self
            .delivery
            .lock()
            .expect("delivery router poisoned")
            .clone()
            .is_some_and(|router| {
                let origin = delivery_origin()
                    .filter(|origin| origin.supervisor == router.supervisor)
                    .map(|origin| origin.push(event.clone()));
                match origin {
                    Some(accepted) => accepted,
                    None => router
                        .external_run
                        .lock()
                        .expect("App::run route poisoned")
                        .clone()
                        .filter(RunContext::accepts)
                        .is_some_and(|run| {
                            router.sender.upgrade().is_some_and(|sender| {
                                sender
                                    .try_send(DeliveryMessage::Event { event, run })
                                    .is_ok()
                            })
                        }),
                }
            });
        observers + usize::from(routed)
    }

    /// Subscribe as an observer to every event published *after* this call. App delivery is owned by
    /// one private supervisor queue, so observation receivers never route triggers or compete with it.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }

    pub(crate) fn observe(&self, event: Event) -> usize {
        self.tx.send(event).unwrap_or(0)
    }

    pub(crate) fn install_delivery_router(
        &self,
        supervisor: u64,
        sender: mpsc::WeakSender<DeliveryMessage>,
        external_run: Arc<Mutex<Option<RunContext>>>,
    ) -> bool {
        let mut delivery = self.delivery.lock().expect("delivery router poisoned");
        if delivery.is_some() {
            return false;
        }
        *delivery = Some(DeliveryRouter {
            supervisor,
            sender,
            external_run,
        });
        true
    }

    /// Record a message a journey sent on a channel (so it can be asserted/rendered).
    pub fn record_send(
        &self,
        channel: impl Into<String>,
        message: impl Into<String>,
        expects_reply: bool,
    ) {
        self.sent.lock().unwrap().push(SentMessage {
            channel: channel.into(),
            message: message.into(),
            expects_reply,
        });
    }

    /// A snapshot of every message sent so far, in order.
    pub fn sent(&self) -> Vec<SentMessage> {
        self.sent.lock().unwrap().clone()
    }
}

pub(crate) async fn scope_delivery<F>(delivery: DeliveryOrigin, future: F) -> F::Output
where
    F: Future,
{
    ACTIVE_DELIVERY.scope(delivery, future).await
}

pub(crate) fn delivery_origin() -> Option<DeliveryOrigin> {
    ACTIVE_DELIVERY.try_with(Clone::clone).ok()
}

impl Default for Bus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn emit_reaches_a_subscriber() {
        let bus = Bus::new();
        let mut rx = bus.subscribe();
        let got = bus.emit("startup", json!({"k": 1}));
        assert_eq!(got, 1, "one live subscriber received the event");
        let ev = rx.recv().await.unwrap();
        assert_eq!(ev.label, "startup");
        assert_eq!(ev.payload, json!({"k": 1}));
    }

    #[test]
    fn emit_without_subscribers_is_a_noop_not_an_error() {
        let bus = Bus::new();
        assert_eq!(bus.emit("nobody-home", json!(null)), 0);
    }

    #[test]
    fn recorded_sends_are_observable_and_clones_share_the_log() {
        let bus = Bus::new();
        let clone = bus.clone();
        clone.record_send("cli", "hello", false);
        bus.record_send("cli", "question?", true);
        let sent = bus.sent();
        assert_eq!(sent.len(), 2, "both clones write to the same log");
        assert_eq!(sent[0].message, "hello");
        assert!(!sent[0].expects_reply);
        assert!(sent[1].expects_reply);
    }

    #[test]
    fn delivery_router_does_not_keep_a_dropped_command_channel_alive() {
        let bus = Bus::new();
        let (sender, _receiver) = mpsc::channel(CAPACITY);
        let external_run = Arc::new(Mutex::new(None));
        assert!(bus.install_delivery_router(1, sender.downgrade(), external_run.clone(),));
        let (run, _errors) = RunContext::new();
        run.activate();
        *external_run.lock().unwrap() = Some(run);
        drop(sender);

        assert_eq!(bus.emit("tick", json!({})), 0);
    }

    #[test]
    fn delivery_router_applies_bounded_backpressure() {
        let bus = Bus::new();
        let (sender, _receiver) = mpsc::channel(CAPACITY);
        let external_run = Arc::new(Mutex::new(None));
        assert!(bus.install_delivery_router(1, sender.downgrade(), external_run.clone(),));
        let (run, _errors) = RunContext::new();
        run.activate();
        *external_run.lock().unwrap() = Some(run);

        for index in 0..CAPACITY {
            assert_eq!(bus.emit("tick", json!({"index": index})), 1);
        }
        assert_eq!(bus.emit("overflow", json!({})), 0);
    }
}
