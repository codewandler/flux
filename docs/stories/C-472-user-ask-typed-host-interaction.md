---
id: C-472
title: "user.ask — schema-driven questions on an attached human surface"
pillar: Core
status: done
priority:
epic:
design: ../designs/user-interaction.md
note: "A conversational agent can ask a human for a validated JSON value through one surface-owned request/reply seam; the existing app journey ask remains separate"
---

# `user.ask` — schema-driven questions on an attached human surface

## Goal

Give a conversational agent one typed, cancellable way to ask its human for missing information.
The agent supplies a prompt and JSON Schema; an attached host renders the best UI it has and returns
only a reviewed, schema-valid value. This is human interaction, not authorization: it never answers
an approval request or bypasses the execution envelope.

## Acceptance

- [x] A surface-gated `user.ask` operation is advertised only when the host installs a
      `UserInteraction` responder, waits for that responder, and returns an explicit submitted or
      cancelled result. Failing-first: `user_ask_waits_for_a_schema_valid_response`.
- [x] The runtime validates bounded JSON Schema before presentation and validates the response again
      before it reaches the model. Remote references, secret-shaped fields, oversized contracts and
      values that the redactor would change fail closed.
- [x] The plain CLI and TUI render boolean, single-select, multi-select and simple form schemas;
      unsupported-but-valid schemas use a validated raw-JSON editor. Approval chrome and decisions
      remain structurally separate.
- [x] The SDK exports the responder contract and `ClientBuilder::with_user_interaction`; audio is an
      opaque host-asset reference and raw recordings never become a response or durable event.
- [x] Headless, served/A2A, stream-JSON and app surfaces do not advertise `user.ask`; the existing
      `ask(channel, message)` journey operation is unchanged.
- [x] Both operation references, SDK docs, engineering/customer changelogs and their generated mirror
      are synchronized. The full gate is green.

## Progress

- 2026-08-02 — design frozen in `docs/designs/user-interaction.md`; implementation started.
- 2026-08-02 — runtime/tool, SDK, CLI and TUI paths implemented; focused runtime, engine, tool,
  SDK and TUI tests pass. Final workspace gate is pending while concurrent C-473–C-476 work leaves
  unrelated `flux-plugin`/format failures in the shared tree.
- 2026-08-02 — both operation references, SDK and public website documentation, engineering and
  customer changelogs, and the generated website mirror are synchronized; the complete workspace
  gate and both sandbox-posture checks pass with C-473–C-476 integrated.

## Notes

- C-163 can later reuse the responder contract for manifest-gated plugin prompts, but plugin
  callbacks and command registration are outside this story.
- Local audio-device IO is deliberately deferred. The first-party CLI/TUI declare no audio
  capability; an SDK host may implement playback, transcription and reviewed form filling.
