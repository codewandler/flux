//! The in-process event bus: a `tokio::sync::broadcast` channel carrying [`Event`]s, plus a shared
//! record of every message sent on a channel (so a host or a test can observe what a journey produced).
//!
//! "User input is just an event" — a channel read injects a `user_input` event, a clock injects a
//! `cron:*` event, a journey's `emit` op publishes an arbitrary label. Triggers ([`crate::App`]) map
//! a label back to a journey. The bus carries the labels; the supervisor does the routing.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// The broadcast channel depth. Generous so a burst of `emit`s inside one journey is never dropped
/// before the supervisor drains it.
const CAPACITY: usize = 1024;

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
}

impl Bus {
    /// Create a fresh bus with no subscribers.
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(CAPACITY);
        Self {
            tx,
            sent: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Publish an event. Returns the number of live subscribers that received it (`0` when no one is
    /// listening — not an error: a fire-and-forget emit with no trigger bound is simply a no-op).
    pub fn emit(&self, label: impl Into<String>, payload: serde_json::Value) -> usize {
        self.tx.send(Event::new(label, payload)).unwrap_or(0)
    }

    /// Subscribe to every event published *after* this call.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
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
}
