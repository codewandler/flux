//! Shared A2A task transition/finalization kernel.
//!
//! Blocking sends, background sends, and SSE sends have different response transports, but they
//! own the same durable task state machine. Keeping classification and registry finalization here
//! prevents one transport from forgetting a terminal transition or releasing the live-task entry
//! in a different order.

use std::fmt::Display;
use std::sync::Arc;

use flux_a2a::{Message, TaskState};

use super::{publish_transition, RegisteredTask, TaskRegistry};

/// Which terminal signal wins when cancellation and a run error are observed together.
///
/// This preserves the public behavior of the pre-consolidation transports: the synchronous RPC
/// reports an agent failure first, while streamed/background ownership treats a canceled task as
/// canceled even if cancellation also makes the run return an error.
#[derive(Clone, Copy)]
pub(super) enum TerminalPrecedence {
    Failure,
    Cancellation,
}

/// One classified terminal state. `response_message` is returned by the blocking RPC; transition
/// publication intentionally carries messages only for failures because streamed deltas and the
/// durable conversation remain authoritative for successful output.
pub(super) struct Terminal {
    pub(super) state: TaskState,
    pub(super) response_message: Option<Message>,
    pub(super) error: Option<String>,
}

/// Borrows the one live task and owns its shared state-machine mechanics.
pub(super) struct TaskRun<'a> {
    registry: &'a Arc<TaskRegistry>,
    scope: &'a str,
    task: &'a RegisteredTask,
}

impl<'a> TaskRun<'a> {
    pub(super) fn new(
        registry: &'a Arc<TaskRegistry>,
        scope: &'a str,
        task: &'a RegisteredTask,
    ) -> Self {
        Self {
            registry,
            scope,
            task,
        }
    }

    /// Advance a submitted task to working and publish the one non-terminal transition.
    pub(super) fn start_working(&self) {
        self.registry
            .set_state(self.scope, &self.task.session_id, TaskState::Working);
        publish_transition(
            self.registry,
            self.scope,
            &self.task.session_id,
            &self.task.context_id,
            TaskState::Working,
            None,
            false,
        );
    }

    /// Classify a completed engine future without mutating registry state. The transport may emit
    /// its own final response frame before calling [`finish`](Self::finish).
    pub(super) fn classify<E: Display>(
        &self,
        result: std::result::Result<(), E>,
        success_message: Option<Message>,
        precedence: TerminalPrecedence,
    ) -> Terminal {
        let error = result.err().map(|error| error.to_string());
        let canceled = self.task.cancel.is_cancelled();
        let failure_wins = matches!(precedence, TerminalPrecedence::Failure) && error.is_some();

        if failure_wins {
            return Terminal {
                state: TaskState::Failed,
                response_message: error.as_ref().map(Message::agent_text),
                error,
            };
        }
        if canceled {
            return Terminal {
                state: TaskState::Canceled,
                response_message: None,
                error: None,
            };
        }
        if let Some(error) = error {
            return Terminal {
                state: TaskState::Failed,
                response_message: Some(Message::agent_text(&error)),
                error: Some(error),
            };
        }
        Terminal {
            state: TaskState::Completed,
            response_message: success_message,
            error: None,
        }
    }

    /// Publish the durable terminal transition and release the live entry. Call while the outer
    /// turn gate is still held so a queued continuation cannot collide with the finishing task.
    pub(super) fn finish(&self, terminal: &Terminal) {
        let transition_message = terminal.error.as_ref().map(Message::agent_text);
        publish_transition(
            self.registry,
            self.scope,
            &self.task.session_id,
            &self.task.context_id,
            terminal.state,
            transition_message,
            true,
        );
        self.registry.finish(self.scope, &self.task.session_id);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::a2a::LiveTask;

    fn registered(
        registry: &Arc<TaskRegistry>,
        cancel: CancellationToken,
    ) -> (RegisteredTask, tokio::sync::broadcast::Receiver<Value>) {
        let (updates, receiver) = tokio::sync::broadcast::channel(8);
        registry.live.lock().unwrap().insert(
            ("scope".into(), "task".into()),
            LiveTask {
                state: TaskState::Submitted,
                realm: None,
                context_id: "context".into(),
                cancel: cancel.clone(),
                updates,
            },
        );
        (
            RegisteredTask {
                session_id: "task".into(),
                context_id: "context".into(),
                cancel,
            },
            receiver,
        )
    }

    #[tokio::test]
    async fn all_transports_share_working_and_terminal_registry_transitions() {
        let registry = Arc::new(TaskRegistry::with_private_net(
            flux_system::net::PrivateNetAllow::None,
        ));
        let (task, mut updates) = registered(&registry, CancellationToken::new());
        let run = TaskRun::new(&registry, "scope", &task);

        run.start_working();
        let working = updates.recv().await.unwrap();
        assert_eq!(working["status"]["state"], "working");

        let terminal = run.classify(
            Ok::<(), &str>(()),
            Some(Message::agent_text("answer")),
            TerminalPrecedence::Failure,
        );
        assert_eq!(terminal.state, TaskState::Completed);
        run.finish(&terminal);

        let completed = updates.recv().await.unwrap();
        assert_eq!(completed["status"]["state"], "completed");
        assert!(
            registry.snapshot("scope", "task", None).is_none(),
            "terminal task must be released"
        );
    }

    #[test]
    fn transport_precedence_is_explicit_for_simultaneous_cancel_and_error() {
        let registry = Arc::new(TaskRegistry::with_private_net(
            flux_system::net::PrivateNetAllow::None,
        ));
        let cancel = CancellationToken::new();
        cancel.cancel();
        let (task, _updates) = registered(&registry, cancel);
        let run = TaskRun::new(&registry, "scope", &task);

        let blocking = run.classify(Err::<(), _>("boom"), None, TerminalPrecedence::Failure);
        assert_eq!(blocking.state, TaskState::Failed);
        assert_eq!(blocking.error.as_deref(), Some("boom"));

        let streamed = run.classify(Err::<(), _>("boom"), None, TerminalPrecedence::Cancellation);
        assert_eq!(streamed.state, TaskState::Canceled);
        assert!(streamed.error.is_none());
    }
}
