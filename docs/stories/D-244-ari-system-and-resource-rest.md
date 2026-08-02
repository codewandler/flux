---
id: D-244
title: "Ship ARI system, application, endpoint, device-state and mailbox resources"
pillar: Agent
status: done
priority: 4
epic: asterisk-ari
design: docs/designs/asterisk-ari.md
areas: [plugins]
note: "first generated REST wave with exact MockHost request and response fixtures"
---

# Ship ARI system, application, endpoint, device-state and mailbox resources

## Goal

Expose the smaller ARI resource families as the first complete generated REST wave.

## Acceptance

- [x] Every Swagger operation in `asterisk`, `applications`, `endpoints`, `deviceStates` and
      `mailboxes` is present exactly once and has a hermetic representative request/response test.
- [x] Read, mutation and deletion contracts exercise path/query/body encoding and non-2xx errors.
- [x] The scoped Asterisk plugin build, tests, clippy and formatting gate is green.

## Progress

- 2026-08-02 failing first: the new resource census fixture expected no operations and failed with
  the measured 36, before the complete fixture table was supplied.
- The final proof defines 36 distinct per-operation MockHost fixtures: applications 5, asterisk 16,
  endpoints 7, device states 4 and mailboxes 4. It covers 6 DELETEs, 6 JSON bodies, 12 query
  requests, encoded path/query values, 20 model/list responses, 16 void responses and a preserved
  422 status/body.
- Before the shared-tree reset, `cargo test -p asterisk` passed the 42-test target (36 fixtures, two
  D-244 proofs and four included executor tests); build, all-target clippy with `-D warnings`,
  package formatting and scoped diff checks were green. Reconstruction must re-run these gates.
