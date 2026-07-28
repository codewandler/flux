//! Mid-turn steering (A-94): user guidance submitted while a turn is executing.
//!
//! A [`SteeringQueue`] is a surface-owned handle shared with the engine via
//! [`crate::FlowEngine::set_steering`]. The surface pushes (and may still edit, reorder, or
//! retract) queued messages; the adaptive loop drains the queue at the head of every planner
//! consultation round and injects the drained texts into the model conversation as an attributed
//! steering block — without cancelling in-flight operations or disturbing a pending approval.
//!
//! Consumption is the commit point: an item that has been drained can no longer be edited or
//! retracted, and the queue is empty again for the surface's rendering. Nothing here touches the
//! persisted session log — the loop records consumed steering as a `turn.steering` observation,
//! so the durable conversation projection keeps its strict user → assistant alternation.

use std::collections::VecDeque;
use std::sync::Mutex;

/// One queued steering message, identified stably across edits and reorders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteeringItem {
    pub id: u64,
    pub text: String,
}

/// A thread-safe FIFO of pending steering messages shared between a surface and the engine.
#[derive(Debug, Default)]
pub struct SteeringQueue {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    next_id: u64,
    items: VecDeque<SteeringItem>,
}

impl SteeringQueue {
    /// Queue a message; returns its stable id. Blank text is ignored (returns `None`).
    pub fn push(&self, text: impl Into<String>) -> Option<u64> {
        let text = text.into();
        if text.trim().is_empty() {
            return None;
        }
        let mut inner = self.inner.lock().unwrap();
        let id = inner.next_id;
        inner.next_id += 1;
        inner.items.push_back(SteeringItem { id, text });
        Some(id)
    }

    /// Replace the text of a still-queued item. `false` when the item was already consumed
    /// (drained), retracted, or never existed.
    pub fn edit(&self, id: u64, text: impl Into<String>) -> bool {
        let mut inner = self.inner.lock().unwrap();
        match inner.items.iter_mut().find(|item| item.id == id) {
            Some(item) => {
                item.text = text.into();
                true
            }
            None => false,
        }
    }

    /// Remove a still-queued item, returning its text. `None` when it was already consumed.
    pub fn retract(&self, id: u64) -> Option<String> {
        let mut inner = self.inner.lock().unwrap();
        let index = inner.items.iter().position(|item| item.id == id)?;
        inner.items.remove(index).map(|item| item.text)
    }

    /// Move a still-queued item by `delta` positions (clamped to the queue bounds).
    pub fn move_by(&self, id: u64, delta: isize) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let Some(from) = inner.items.iter().position(|item| item.id == id) else {
            return false;
        };
        let to = from
            .saturating_add_signed(delta)
            .min(inner.items.len().saturating_sub(1));
        if from == to {
            return false;
        }
        inner.items.swap(from, to);
        true
    }

    /// A point-in-time copy for rendering. The queue may be drained concurrently by the engine;
    /// treat indices into a snapshot as hints and re-resolve by id.
    pub fn snapshot(&self) -> Vec<SteeringItem> {
        self.inner.lock().unwrap().items.iter().cloned().collect()
    }

    /// Consume every queued message in FIFO order. This is the engine's commit point: drained
    /// items can no longer be edited or retracted.
    pub fn drain(&self) -> Vec<String> {
        let mut inner = self.inner.lock().unwrap();
        inner.items.drain(..).map(|item| item.text).collect()
    }

    /// Consume only the oldest queued message (the surface's idle-time drain: leftovers after a
    /// turn finishes become ordinary follow-up turns, front first).
    pub fn pop_front(&self) -> Option<String> {
        let mut inner = self.inner.lock().unwrap();
        inner.items.pop_front().map(|item| item.text)
    }

    pub fn clear(&self) {
        self.inner.lock().unwrap().items.clear();
    }

    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.lock().unwrap().items.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_edit_retract_and_reorder_track_ids_not_positions() {
        let queue = SteeringQueue::default();
        assert_eq!(queue.push("   "), None);
        let a = queue.push("first").unwrap();
        let b = queue.push("second").unwrap();
        let c = queue.push("third").unwrap();
        assert_eq!(queue.len(), 3);

        assert!(queue.edit(b, "second (edited)"));
        assert!(queue.move_by(c, -2));
        assert_eq!(
            queue
                .snapshot()
                .iter()
                .map(|item| item.text.as_str())
                .collect::<Vec<_>>(),
            vec!["third", "second (edited)", "first"]
        );

        assert_eq!(queue.retract(a), Some("first".into()));
        assert_eq!(queue.retract(a), None);
        assert_eq!(queue.drain(), vec!["third", "second (edited)"]);
        assert!(queue.is_empty());
    }

    #[test]
    fn drained_items_are_consumed_and_immutable() {
        let queue = SteeringQueue::default();
        let id = queue.push("go left").unwrap();
        assert_eq!(queue.drain(), vec!["go left".to_string()]);
        assert!(!queue.edit(id, "go right"));
        assert_eq!(queue.retract(id), None);
        assert_eq!(queue.pop_front(), None);
    }

    #[test]
    fn pop_front_consumes_fifo() {
        let queue = SteeringQueue::default();
        queue.push("one");
        queue.push("two");
        assert_eq!(queue.pop_front(), Some("one".into()));
        assert_eq!(queue.pop_front(), Some("two".into()));
        assert_eq!(queue.pop_front(), None);
    }
}
