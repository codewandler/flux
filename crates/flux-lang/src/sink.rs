//! The observation **sink** the interpreter streams to as it executes — the language-level mirror of
//! the engine's `AgentSink`. Op results are reported as [`OpOutcome`]s (not the host's tool result),
//! keeping the trait free of any runtime type. The engine bridges this to its real sink.

use flux_core::DispatchId;

use crate::host::OpOutcome;

/// A streaming sink for interpreter observations (text/thinking deltas, op calls/results, audit
/// observations, turn end). Every method defaults to a no-op, so a caller overrides only what it
/// needs — a headless run uses the empty default.
pub trait FlowSink: Send {
    fn text_delta(&mut self, _text: &str) {}
    fn thinking_delta(&mut self, _text: &str) {}
    /// The planner is composing (`true`) / has finished (`false`).
    fn planning(&mut self, _active: bool) {}
    /// An op is being dispatched. `dispatch` is minted per call and repeated on the matching
    /// [`Self::tool_result`], so a consumer pairs the two by identity rather than by name and
    /// arrival order — concurrent same-name calls may complete in any order (C-531).
    fn tool_call(&mut self, _dispatch: DispatchId, _name: &str, _input: &serde_json::Value) {}
    /// The op named by `dispatch` returned.
    fn tool_result(&mut self, _dispatch: DispatchId, _name: &str, _result: &OpOutcome) {}
    /// An audit observation made during dispatch (e.g. a destructive-command marker).
    fn observation(&mut self, _o: &flux_evidence::Observation) {}
    fn turn_end(&mut self, _usage: Option<flux_core::Usage>) {}
    /// Whether the interpreter should emit live structural-trace observations
    /// (`loop.round` / `loop.node`) for this execution. Default off; a host opts in
    /// per execution via the sink it passes to `execute_flow`.
    fn trace_structural(&self) -> bool {
        false
    }
}
