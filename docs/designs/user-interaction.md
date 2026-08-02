# Typed user interaction

**Status:** accepted · **Pillar:** Core · **Story:**
[C-472](../stories/C-472-user-ask-typed-host-interaction.md)

## Decision

`user.ask` is an ordinary operation over a new request/reply capability installed by the human
surface. It does not reuse `Approver`: answering a product question is not authorization, and no
interaction response can mint an approval choice. It also does not reuse the pane `SurfaceSink`,
which is intentionally synchronous and send-only.

The L2 runtime owns the value contract and its redacting, validating reporter. L6 hosts own
rendering and input. `flux-tools` contributes `user.ask` only when assembly proves a responder is
present, exactly as pane operations are conditional on a surface sink. The responder is carried in
the engine's lexical `RuntimeTurnContext`; a retained context cannot revive a responder from an old
turn.

The existing `flux-app` operation `ask(channel, message)` remains an orchestration primitive that
parks a journey for a channel reply. Giving that public name a second meaning would make catalogs
and authored flows context-dependent, so the conversational operation is `user.ask`.

## Wire and validation

The operation accepts `prompt: { text, audio? }` and `schema`. Audio is
`{ asset_id, media_type, transcript }`: an opaque host-owned reference, never bytes, a filesystem
path or a URL. A capable SDK host resolves the id; the stock CLI/TUI reject audio-bearing requests.
Recorded replies stay inside the host. The response contains only the reviewed JSON value and
`input_mode: controls | audio | mixed`.

The runtime accepts bounded object/boolean JSON Schemas, declared drafts and local `$ref`/`$defs`.
It rejects remote references, more than 64 KiB of schema, depth over 32, more than 128 form
fields/options, more than 8 KiB of prompt or transcript, and responses over 16 KiB. It also rejects
`writeOnly`, `format: password`, and secret-shaped property names. Before a response crosses into a
tool result, serializing it through the session redactor must be a no-op; otherwise the response is
refused instead of returning malformed or partially-redacted JSON.

The runtime validates once before presentation and again after submission. A bad SDK responder is
a tool error. First-party surfaces keep the form open and show the validation error, so invalid
input never leaves the surface.

## Rendering and lifecycle

- Boolean schemas render yes/no.
- String enums or titled `oneOf` constants render a single select.
- Unique arrays of enum items render a multi-select.
- Scalar roots and flat objects of those fields render native controls using standard title,
  description, required and default annotations.
- Every other valid schema renders as a raw JSON editor backed by the same validator.

Concurrent questions queue FIFO. Approval has higher modal/key priority in the TUI, and the plain
terminal serializes approval and interaction reads through one prompt lock. Human cancellation is
a successful `{status:"cancelled"}` result; turn cancellation removes the live/queued request; a
dropped surface is an actionable error. There is no model-selected timeout.

Prompt text and audio transcripts are redacted before the responder sees them and rendered as
untrusted plain text under host-owned "Agent question" chrome. The interaction modal cannot imitate
or answer the approval sheet.

## Surface boundary

The first release wires `flux run`, the REPL, the TUI and `ClientBuilder`. Stream-JSON owns stdin as
a framed protocol, served/A2A has no elicitation wire, `flux app run` already has channel parking,
and local audio has no guarded device backend; those surfaces do not register `user.ask`. Plugin
host prompts remain C-163 and will carry a runtime-owned plugin origin when they reuse this seam.
