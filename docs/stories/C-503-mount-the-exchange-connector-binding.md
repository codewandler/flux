---
id: C-503
title: "Embed the Exchange connector client"
pillar: Core
status: in-progress
priority: 0
epic: connector-native-integrations
design: docs/designs/ecosystem.md
note: "Milestone 1 only: embed Service Account auth, effective-catalogue refresh and one-shot HTTP invoke; lifecycle frames follow later"
---

# Embed the Exchange connector client

## Goal

Make Exchange the compiled-in official integration path: Flux authenticates as one Service Account,
projects its effective catalogue into the existing tool registry at turn boundaries, and invokes the
existing one-shot HTTP operation path without learning vendor authority.

## Acceptance

- [x] One native Rust client is compiled into the core Flux binary; there is no helper executable,
      plugin, installed pack dependency, caller-selected placement, or local official fallback.
- [x] The client authenticates with Exchange's canonical Service Account API and derives no tenant,
      credential, endpoint, connection, grant, or runtime from model-controlled input.
- [x] An authenticated effective catalogue exposes only connected and granted operations for that
      Service Account, carries a stable generation identity, and refreshes the model-facing registry
      only between turns through C-318's delivered seam.
- [x] One-shot operations use the existing HTTP `invoke` contract and preserve distinct authentication,
      grant refusal, unavailable Exchange, and Exchange runtime failure outcomes.
- [x] Failing-first integration tests prove an Exchange-held credential never enters Flux output,
      logs, events or persisted session state, and that an unavailable Exchange removes only official
      external tools rather than core Flux capabilities.
- [x] Subscribe, streamed output, cancellation frames, terminal lifecycle and leases are explicitly
      outside this Milestone 1 story and receive their own contract in the lifecycle milestone.

## Progress

- 2026-08-03: implementation started in the `flux-core-2` wave after verifying Exchange X-113 and
  Flux C-318 are both merged on their canonical main branches.
- 2026-08-03: failing-first evidence covered the missing native Exchange module and engine refresh
  wiring; `cargo test -p codewandler-flux-web exchange::tests`, the focused flux-flow boundary test,
  all `flux-cli` targets, and the repaired website contract pass. The wave-level repository gate is
  pending integration with the remaining stories.
- 2026-08-04: coordinator audit remediation preserves X-113's structured `refusal`/`code`, `sent`
  and `retryable` fields (including vendor `transport` versus Exchange unavailability and
  disconnected/ambiguous connections), refuses cleartext bearer transport beyond loopback, and
  adds a real-CLI persistence proof backed by a faithful Exchange-host credential store. The proof
  consumes the held sentinel on the vendor wire, then excludes it from Flux output/logs, Exchange
  logs, Flux/Exchange wire, events, evidence, conversation, run trace and store bytes. Focused tests,
  focused all-target clippy, formatting and website contracts pass; the wave gate remains pending.

## Notes

- Depends on C-318 plus Exchange X-107 and the Milestone 1 effective-catalogue/one-shot HTTP slice.
- X-107 already delivers canonical Service Account authentication; this story consumes it and does
  not duplicate lifecycle or bearer verification.
