---
id: A-51
title: Inbound multimodal parts — accept file/data Parts or refuse cleanly
pillar: Agent
status: done
epic: a2a-conformance
design: docs/designs/a2a-conformance.md
note: "Tier-2: file/data input parts are silently dropped in extract_text; the turn runs on empty input"
---

# Inbound multimodal parts

## Goal
Handle A2A `file` and `data` message parts on inbound requests instead of silently discarding them —
either by surfacing their content to the agent turn, or (until multimodal input is wired end-to-end)
by refusing the request cleanly with the correct error code, so a client is never left thinking a
file it sent was processed.

## Why (evidence)
- `crates/flux-a2a/src/server.rs:195-208` — `extract_text` keeps only `kind == "text"` parts;
  `file`/`data` parts round-trip through `Part.extra` (`types.rs:56-57`) but are never read, so a
  message that is *only* a file/data part runs the turn on empty input with no signal to the client.

## Acceptance
- [ ] Inbound `file`/`data` parts are either (a) surfaced into the turn input in a defined way, or
      (b) rejected with `-32005 ContentTypeNotSupported` (depends on A-50 landing the code) when the
      agent can't consume them — decided in this story, not left silent.
- [ ] `Part` gains first-class `file`/`data` accessors as needed (today they live only in `extra`).
- [ ] Failing-first test: a `message/send` whose only part is a `data`/`file` part produces the chosen
      defined behavior (surfaced or `-32005`), not an empty-input completed Task.
- [ ] Docs: the Message & Parts rows in the support matrix update.

## Progress
- **Done.** Scope decision, per part kind, made here: **`data` → accept, `file` → refuse.**
- The old `extract_text` + `no_text_error_code` split at the dispatch sites was replaced by a single
  shared boundary `flux_a2a::server::extract_input(params) -> Result<String, i32>`, used by the
  reusable `dispatch` and both `flux-server` HTTP handlers (`send`/`subscribe`) so the "what do we
  run / how do we refuse" decision lives in exactly one place and cannot drift.
  - `text` parts contribute their text (as before).
  - `data` parts are **surfaced** into the turn input as labeled compact JSON (`render_data_part`),
    so a message whose only part is a `data` part now runs a *real* turn — the agent sees the payload
    — instead of the empty turn `extract_text` alone produced.
  - `file` parts are **refused** with `-32005 ContentTypeNotSupported` — flux's turn is text-only and
    cannot consume file bytes, so a file is never silently dropped, even when it rides alongside text.
  - Refusal codes: absent message / empty parts → `-32602`; parts present but none usable (lone
    `file`, or only unknown kinds) → `-32005`.
- `Part` gained first-class `as_data()` / `as_file()` accessors (previously reachable only via
  `Part::extra`).
- **Tests (failing-first):** `flux_a2a::server` unit tests `extract_input_composes_text_and_surfaces_data_parts`
  and `extract_input_refuses_files_and_malformed`; `flux-server` integration test
  `inbound_data_part_is_surfaced_and_file_part_is_refused` (data-only → `completed`; text+file →
  `-32005`) over the real router. The pre-existing A-50 `no_usable_text_*` expectations still hold.
- **Docs:** the Message & Parts rows (contributor + website matrix) updated; epic design records the
  per-kind scope decision.

## Notes
- Sequenced after A-50 (uses `-32005`). Scope decision (accept vs refuse) belongs in this story's
  design step. Epic: [a2a-conformance](../designs/a2a-conformance.md).
