---
id: C-503
title: "Embed the Exchange connector client"
pillar: Core
status: backlog
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

- [ ] One native Rust client is compiled into the core Flux binary; there is no helper executable,
      plugin, installed pack dependency, caller-selected placement, or local official fallback.
- [ ] The client authenticates with Exchange's canonical Service Account API and derives no tenant,
      credential, endpoint, connection, grant, or runtime from model-controlled input.
- [ ] An authenticated effective catalogue exposes only connected and granted operations for that
      Service Account, carries a stable generation identity, and refreshes the model-facing registry
      only between turns through C-318's delivered seam.
- [ ] One-shot operations use the existing HTTP `invoke` contract and preserve distinct authentication,
      grant refusal, unavailable Exchange, and Exchange runtime failure outcomes.
- [ ] Failing-first integration tests prove an Exchange-held credential never enters Flux output,
      logs, events or persisted session state, and that an unavailable Exchange removes only official
      external tools rather than core Flux capabilities.
- [ ] Subscribe, streamed output, cancellation frames, terminal lifecycle and leases are explicitly
      outside this Milestone 1 story and receive their own contract in the lifecycle milestone.

## Progress

- (not started)

## Notes

- Depends on C-318 plus Exchange X-107 and the Milestone 1 effective-catalogue/one-shot HTTP slice.
- X-107 already delivers canonical Service Account authentication; this story consumes it and does
  not duplicate lifecycle or bearer verification.
