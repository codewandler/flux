---
id: C-199
title: "Document the HTTP session API — twelve routes ship, three are documented"
pillar: Core
status: done
priority: 11
epic: website-truth-and-identity
design: docs/designs/website-truth-and-identity.md
note: "flux-server registers 12 routes; agent/a2a.md:69-73 documents the 3 A2A ones — POST /sessions, the SSE stream, /webhook and both usage endpoints appear nowhere on the site, and two aren't in the README either"
---

# Document the HTTP session API — twelve routes ship, three are documented

## Goal
`crates/flux-server/src/lib.rs:585-607` (single-agent router) and `:766-776` (multi-agent) register
twelve public routes. The website documents three of them, in `agent/a2a.md:69-73`. The session
REST surface, its SSE stream and the webhook — the reason `flux app run --serve` exists — are
publicly undocumented, and `GET /sessions/{id}/usage` and `GET /usage` are missing from the README
as well. Give the server a reference page, and make an undocumented route fail the gate.

## Acceptance
- [x] New `website/docs/agent/http-api.md` documents every registered public route — **14** once
      the multi-agent mount is counted, not the 12 the story estimated (see Progress).
- [x] The SSE event table (`text` / `tool` / `error` / `done`) and every request/response example
      are taken from the handlers, not invented.
- [x] The multi-agent mount gets its own route table, and the fact that it exposes **A2A only** —
      no per-agent session subtree — is stated rather than left to inference.
- [x] Auth is cross-linked to `security/server-auth.md`, not restated.
- [x] Wired into `sidebars.js` under the Agent category; `npm run build` clean.
- [x] Failing-first: new `http_api_reference_covers_every_served_route` in
      `crates/flux-cli/tests/website_contract.rs`.

## Progress
- **The route set was bigger than the audit's.** The audit counted 12; extracting `.route(` literals
  from the production half of `crates/flux-server/src/lib.rs` recovers **14** — the two extra are
  `/{agent_id}/.well-known/agent-card.json` and `/{agent_id}/.well-known/agent.json` on the
  multi-agent mount. They were missed because `rustfmt` wraps those two calls, putting the path
  literal on the line *after* `.route(`. The first draft of the coverage test made the same mistake
  (`match_indices(".route(\"")`); it now skips whitespace after `.route(` instead of assuming the
  quote is adjacent, which is what caught the omission in my own page draft.
- **Test source of truth.** Axum's `Router` cannot be enumerated at runtime, so the guard reads the
  route literals out of the source — the same shape as `cli_reference_covers_every_public_subcommand`
  reading `--help`. It splits at `#[cfg(test)]` first, because the file's own test module mounts
  throwaway routers (`/protected`) that must not be treated as public surface. A `>= 12` floor
  guards against the extraction silently returning nothing and the loop passing vacuously.
- Failing-first is structural here: the page did not exist at HEAD, so the test's `read()` panics
  before reaching any assertion.
- **Documented from the handlers.** `usage` reports all five token tiers (`usage_json`), and is
  `null` when the provider reported none. The SSE path notably emits **no** usage summary —
  `SseSink` only implements `text_delta` and `tool_call`, not `turn_end` — so the page tells callers
  to read `GET /sessions/{id}/usage` afterwards rather than leaving them to discover the gap.
  `/webhook` returns `session_id` + `text` + `tool_calls` and, unlike `POST …/messages`, no usage.
- **Documented the C-189 limits, which landed mid-story.** The router carries `DefaultBodyLimit` and
  a `TimeoutLayer` with `FLUX_SERVER_MAX_BODY_BYTES` / `FLUX_SERVER_REQUEST_TIMEOUT_SECS` overrides,
  so the page documents `413`, `408` and the deliberate SSE exemption (the timeout bounds response
  *production*, not body streaming). A first pass flagged this as a board discrepancy — the source
  cited C-189 while the board still showed C-189 `ready`. Re-derived rather than filed: a sibling
  session closed C-189 in `713ff60` while this story was in progress, and the board was already
  current. No discrepancy; the page simply documents behaviour that became true underneath it.
- Gate: `cargo test -p flux-cli --test website_contract` — 15 green. `npm run build` clean.

## Notes
- The page documents the body cap and timeout as behaviour, not as configuration to tune: the
  defaults are deliberately generous and the env overrides exist for deployments, so the reference
  states the status codes a client must handle and leaves the rationale to C-189's own story.

## Notes
- The route list must be derived from the router source, not from the README — the README is
  already known to be behind (it omits both usage endpoints).
- `agent/a2a.md` keeps ownership of the A2A protocol semantics; the new page lists the A2A routes
  as part of the surface and links across rather than duplicating the conformance detail in
  `agent/a2a-conformance.md`.
