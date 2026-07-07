---
id: A-50
title: A2A-specific JSON-RPC error codes — UnsupportedOperation / ContentTypeNotSupported (and the -32001..-32007 set)
pillar: Agent
status: done
epic: a2a-conformance
design: docs/designs/a2a-conformance.md
note: "Tier-1 quick-win: unsupported A2A methods return generic -32601 and non-text input is silently dropped; the A2A binding defines dedicated codes"
---

# A2A-specific JSON-RPC error codes

## Goal
Return the A2A protocol's own JSON-RPC error codes where they carry more meaning than the generic
base codes, so conformant clients can distinguish "this A2A method isn't supported here" and "I can't
use this content type" from a blanket method-not-found / silent success.

## Why (evidence)
- Every unknown/unsupported method falls through to `-32601` "Method not found" — reusable dispatch
  `crates/flux-a2a/src/server.rs:149-156`; HTTP dispatch `crates/flux-server/src/a2a.rs:355-377` and
  the multi-agent variant `a2a.rs:274-295`. A2A defines `-32004 UnsupportedOperationError` for a
  *known* operation a server chose not to implement — more accurate for `tasks/cancel`,
  `tasks/resubscribe`, `tasks/pushNotificationConfig/*`, `agent/getAuthenticatedExtendedCard`.
- Inbound messages with no usable text part are silently dropped (`extract_text` ignores non-text
  parts, `server.rs:195-208`) and the turn runs on empty input; A2A defines
  `-32005 ContentTypeNotSupportedError` for exactly this.
- flux emits only base JSON-RPC codes today (`-32600/-32601/-32602/-32603`); none of `-32001..-32007`.

## Acceptance
- [ ] Add the A2A error-code constants to `flux-a2a` (the JSON-RPC binding set):
      `-32001 TaskNotFound`, `-32002 TaskNotCancelable`, `-32003 PushNotificationNotSupported`,
      `-32004 UnsupportedOperation`, `-32005 ContentTypeNotSupported`, `-32006 InvalidAgentResponse`,
      `-32007 AuthenticatedExtendedCardNotConfigured` — as named consts with doc comments, reused by
      both dispatchers.
- [ ] Defined-but-unsupported A2A methods (`tasks/cancel`, `tasks/resubscribe`,
      `tasks/pushNotificationConfig/{set,get,list,delete}`, `agent/getAuthenticatedExtendedCard`)
      return `-32004 UnsupportedOperation` (not `-32601`). Genuinely-unknown method names keep
      `-32601` (correct JSON-RPC). Applied in BOTH `flux-a2a::server::dispatch` and the
      `flux-server` HTTP dispatch(es) — name the drift risk in the test.
- [ ] A `message/send`/`message/stream` whose message carries no text part returns
      `-32005 ContentTypeNotSupported` instead of running an empty turn. (This is a prerequisite-shaped
      sibling of A-51, which decides whether to *accept* file/data parts; until then, refusing is the
      honest behavior.)
- [ ] Failing-first tests: `dispatch` returns `-32004` for a `tasks/cancel` request and `-32005` for a
      no-text message; a genuinely-unknown method still returns `-32601`. Cover both dispatch sites.
- [ ] Docs: the error-code table in the support matrix reflects the newly-emitted codes.

## Progress
- 2026-07-07 done. Added the `-32001..-32007` constants as a new `flux_a2a::error` module (doc
  comments distinguish the two flux emits today from the task-lifecycle codes reserved for A-53). Two
  shared classifiers in `flux_a2a::server` are the anti-drift mechanism: `is_unsupported_a2a_method`
  (matches `tasks/cancel`, `tasks/resubscribe`, `tasks/pushNotificationConfig/{set,get,list,delete}`,
  `agent/getAuthenticatedExtendedCard` — `tasks/get` deliberately excluded, its correct code is
  task-retention-dependent) → `-32004`; and `no_text_error_code` → `-32005` when a message carries
  parts but no text, else `-32602` (empty/absent parts stays invalid-params, preserving the existing
  test). Wired into all three dispatch sites: `flux_a2a::server::dispatch` + the two `flux-server`
  HTTP handlers (`a2a_handler`, `a2a_handler_multi`); `subscribe`'s error type became `(i32, String)`
  so streaming carries the A2A code too. Tests cover both dispatch sites (`flux-a2a` unit +
  `flux-server` `error_codes_on_the_single_agent_dispatcher` / `unsupported_method_on_the_multi_agent_dispatcher`).
  Full workspace gate green.

## Notes
- Numeric codes are from the A2A JSON-RPC binding (stable; used by the a2a-python / a2a-js SDKs); the
  protobuf spec view lists the names without codes.
- Additive/non-breaking: these only replace today's generic codes on already-failing paths.
- Epic: [a2a-conformance](../designs/a2a-conformance.md).
