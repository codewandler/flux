//! Turn-segmented transcript accumulation for a voice session (C-38).
//!
//! A plain helper — **not** a [`super::VoiceSink`] decorator. A consumer's own `VoiceSink`
//! implementation holds one, feeding it from `input_transcript`/`output_transcript` and draining it
//! with [`TranscriptAccumulator::flush`] on `response_done` **and once more at session close** (a
//! hangup can land before the model's final `response.done`, so the close-flush is what keeps the
//! last in-flight turn from being silently dropped).

use flux_core::Message;

/// Buffers one turn's caller/model transcript deltas and segments them into [`Message`]s on
/// [`Self::flush`].
///
/// Caveat: the driver's [`RealtimeEvent::InputTranscriptDelta` and
/// `InputTranscriptDone`](flux_provider::RealtimeEvent) both currently route to
/// `VoiceSink::input_transcript` as one callback (see `flux-flow`'s voice driver). If input
/// transcription is ever enabled with the model emitting BOTH event kinds for the same turn,
/// feeding both into [`Self::push_input`] would double-count the transcript text. The eventual fix
/// is a distinct "input done" callback on the sink; out of scope here.
#[derive(Debug, Default)]
pub struct TranscriptAccumulator {
    input: String,
    output: String,
}

impl TranscriptAccumulator {
    /// A fresh, empty accumulator.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a delta of the caller's input transcript for the current turn.
    pub fn push_input(&mut self, delta: &str) {
        self.input.push_str(delta);
    }

    /// Append a delta of the model's output transcript (or text output) for the current turn.
    pub fn push_output(&mut self, delta: &str) {
        self.output.push_str(delta);
    }

    /// `true` when nothing has been buffered since the last [`Self::flush`].
    pub fn is_empty(&self) -> bool {
        self.input.is_empty() && self.output.is_empty()
    }

    /// Drain the buffered turn into its `Message`s — a user message first (if any input was
    /// buffered), then an assistant message (if any output was buffered); an empty side is skipped
    /// rather than emitted as an empty message. Idempotent: calling `flush` again before any new
    /// `push_*` returns an empty `Vec` (the buffers are already drained), so it is safe to call both
    /// on `response_done` and again at session close without duplicating the same turn.
    pub fn flush(&mut self) -> Vec<Message> {
        let mut out = Vec::with_capacity(2);
        if !self.input.is_empty() {
            out.push(Message::user_text(std::mem::take(&mut self.input)));
        }
        if !self.output.is_empty() {
            out.push(Message::assistant_text(std::mem::take(&mut self.output)));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_core::Role;

    /// A turn with both caller and model speech segments into a user message (input, in push
    /// order) followed by an assistant message (output) — the boundary is the `flush` call, not any
    /// timing signal.
    #[test]
    fn flush_segments_one_turn_into_user_then_assistant() {
        let mut acc = TranscriptAccumulator::new();
        acc.push_input("book a ");
        acc.push_input("table");
        acc.push_output("sure, ");
        acc.push_output("for how many?");

        let msgs = acc.flush();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, Role::User);
        assert_eq!(msgs[0].text(), "book a table");
        assert_eq!(msgs[1].role, Role::Assistant);
        assert_eq!(msgs[1].text(), "sure, for how many?");
    }

    /// After a flush, pushing a NEW turn's deltas and flushing again yields only that new turn —
    /// the prior turn's text must not resurface (turn-boundary correctness).
    #[test]
    fn flush_resets_for_the_next_turn_boundary() {
        let mut acc = TranscriptAccumulator::new();
        acc.push_input("first turn");
        acc.push_output("first reply");
        let first = acc.flush();
        assert_eq!(first.len(), 2);

        acc.push_input("second turn");
        let second = acc.flush();
        assert_eq!(
            second.len(),
            1,
            "only the new turn's input, no assistant side"
        );
        assert_eq!(second[0].text(), "second turn");
    }

    /// A model-initiated turn with no caller input (e.g. a proactive greeting) must skip the empty
    /// user side rather than emit an empty message.
    #[test]
    fn flush_skips_empty_side_for_output_only_turn() {
        let mut acc = TranscriptAccumulator::new();
        acc.push_output("hi, how can I help?");

        let msgs = acc.flush();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, Role::Assistant);
    }

    /// Flushing an accumulator that buffered nothing returns an empty `Vec` — never a pair of empty
    /// messages.
    #[test]
    fn flush_on_empty_accumulator_returns_nothing() {
        let mut acc = TranscriptAccumulator::new();
        assert!(acc.is_empty());
        assert_eq!(acc.flush(), Vec::new());
    }

    /// Idempotence: a second flush right after the first (the close-flush, when `response_done`
    /// already flushed this turn) must not re-emit the same turn — this is what makes it safe to
    /// call `flush` unconditionally at session close even when the last response.done already did.
    #[test]
    fn flush_is_idempotent_across_a_close_flush() {
        let mut acc = TranscriptAccumulator::new();
        acc.push_input("only turn");
        acc.push_output("only reply");

        let on_response_done = acc.flush();
        assert_eq!(on_response_done.len(), 2);

        // Simulate the close-flush: no new pushes happened in between.
        let on_close = acc.flush();
        assert_eq!(on_close, Vec::new());
        assert!(acc.is_empty());
    }
}
