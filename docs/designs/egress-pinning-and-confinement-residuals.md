# Egress pinning and confinement residuals — 2026-08-01

## Context

A seven-way validation pass over [`docs/reviews/aggregate/2026-08-01-aggregate-complaint-triage.md`](../reviews/aggregate/2026-08-01-aggregate-complaint-triage.md)
re-checked every claim in the ledger against the current tree. `NET-01`, `NET-02`, `PROC-01` and
`PROC-02` are **historical-fixed**: C-256/C-257 bind plugin HTTP, OAuth refresh, raw TCP and every
fleet A2A client to the addresses the guard vetted, and the regression tests reach real listeners
through hostnames that have no system DNS entry — they cannot pass if the pinning is removed.

What the pass found instead is that the fix was applied **per reviewed adapter, not per outer
adapter**. Three egress paths that no reviewer inventoried still resolve twice, one of them while
carrying a credential. The same shape appears in confinement: C-262's fail-closed profile is a
`flux-cli` property, and the surfaces below and beside the CLI reproduce the original posture.

## Finding-to-story traceability

| Residual (validated 2026-08-01) | Story |
| --- | --- |
| A2A push-notification delivery is guarded but not pinned, on a pooled client, while sending `X-A2A-Notification-Token` | C-346 |
| `web.browser` CDP egress is guarded only at the URL; Chrome resolves again, and a third resolution writes the audit record | C-347 |
| Unpinned egress helpers remain public and default; `flux a2a <URL>` uses the one unguarded `A2aClient::new`; fleet pinning is proven at the constructor, not through the registered op | C-348 |
| `core.fsmonitor` executes an arbitrary program under `git_status` and `git_diff` — a seam `--no-ext-diff --no-textconv` does not close | C-349 |
| `flux-sdk`/`flux-server` embedders and un-flagged `flux app run` daemons get `Off`/network-open with no preflight, while the docs claim every serving surface uses `require` | C-350 |
| `eval_run` executes undeclared model-reachable parameters (unbounded `trials`), discards its `ToolContext`, and builds its System over the host cwd | C-351 |

## Decisions

- **A guard that returns a URL is not a boundary.** The URL-returning API (`guard_url_scoped`,
  `guard_http_url`) may pre-check, but the connection must be established over the vetted
  `SocketAddr` set. Any outer adapter that cannot consume a pinned client is a documented exemption
  with a named owner, not an oversight.
- **One resolution per authorized connection.** Where an adapter resolves for the guard, for the
  audit record, and again for the socket, the audit record can disagree with what was contacted.
  Collapse to one answer or record the answer that was actually dialled.
- **A fixed-argv exemption states the seams it closes.** `flux-spec`'s I1 exemption reasons are a
  claim about which config-directed execution paths are shut. `core.fsmonitor` proves the audit
  behind those strings was scoped to diff drivers; the reasons must name the full closed set.
- **The confinement floor belongs to the assembly, not the CLI.** An embedder standing up
  `flux-server` is an unattended serving surface by definition. Either it resolves a fail-closed
  posture, or the documentation stops claiming it does.
- **A model-facing op executes only what it declares.** An undeclared parameter that reaches
  process spawn or provider spend is a widening the schema does not disclose.

## Closure proof

Re-run the egress rebinding fixtures against every adapter named in the exemption inventory, and
re-derive the sandbox truth table from the assembled binaries — CLI, SDK embedder, and `flux app run`
without `--yes`. The epic closes when the exemption inventory is complete and each entry is either
pinned or explicitly owned.
