//! Voice session driver — binds a full-duplex realtime (voice-to-voice) session to the runtime.
//!
//! A realtime model (a [`flux_provider::RealtimeProvider`]) owns the acoustic conversation; this
//! module owns the *logic + safety* half: it routes the model's tool calls through the runtime's
//! `Executor` envelope and declares those tools **once** from the live registry. It is the L3 binding
//! that the L1 session seam and the L2 executor meet at — the seam itself never names a runtime type.

mod driver;
mod sink;
#[cfg(test)]
mod tests;

pub use driver::{VoiceSessionDriver, VoiceTurnHandler};
pub use sink::VoiceSink;

use flux_provider::ToolDef;
use flux_runtime::ToolRegistry;

/// Build the realtime session's function declarations from the live tool registry — the single
/// source of truth, so a tool is declared **once** (not once model-facing and once as a runtime op).
pub fn tool_defs_from_registry(registry: &ToolRegistry) -> Vec<ToolDef> {
    registry
        .specs()
        .into_iter()
        .map(|s| ToolDef {
            name: s.name,
            description: s.description,
            input_schema: s.input_schema,
        })
        .collect()
}
