//! The observation **sink** the interpreter streams to as it executes — the language-level mirror of
//! the engine's `AgentSink`. Op results are reported as [`OpOutcome`]s (not the host's tool result),
//! keeping the trait free of any runtime type. The engine bridges this to its real sink.

use crate::host::OpOutcome;

/// A streaming sink for interpreter observations (text/thinking deltas, op calls/results, audit
/// observations, turn end). Every method defaults to a no-op, so a caller overrides only what it
/// needs — a headless run uses the empty default.
pub trait FlowSink: Send {
    fn text_delta(&mut self, _text: &str) {}
    fn thinking_delta(&mut self, _text: &str) {}
    /// The planner is composing (`true`) / has finished (`false`).
    fn planning(&mut self, _active: bool) {}
    fn tool_call(&mut self, _name: &str, _input: &serde_json::Value) {}
    fn tool_result(&mut self, _name: &str, _result: &OpOutcome) {}
    /// An audit observation made during dispatch (e.g. a destructive-command marker).
    fn observation(&mut self, _o: &flux_evidence::Observation) {}
    fn turn_end(&mut self, _usage: Option<flux_core::Usage>) {}
}
