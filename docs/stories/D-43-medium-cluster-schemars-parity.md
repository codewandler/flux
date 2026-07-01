---
id: D-43
title: medium cluster schemars migration + parity ports (huggingface/opsgenie/docker)
pillar: Agent
status: done
priority:
epic:
design:
note: full D-36 per-plugin loop for the 3 medium plugins — schemars migration + fluxplane parity re-audit + ported gaps (3 workers in parallel); docker (33 ops, largest of the batch)
---

# medium cluster schemars migration + parity ports

## Goal

Full D-36 per-plugin loop for huggingface, opsgenie, docker (run in parallel). Each: schemars
migration of every op, a fluxplane parity re-audit, and porting the gaps surfaced.

## Acceptance
- [x] **huggingface** (9 ops): schemars-derived (shared `SearchInput` flattened into
      model/dataset/space search); `so()` deleted; `schema_contract` test. Ported: `chat.stop` +
      `embed.input` enforced as `[]string` (Go rejects non-string elements; flux forwarded
      arbitrary arrays).
- [x] **opsgenie** (8 ops): schemars-derived; `so()` deleted; `schema_contract` test. Ported:
      `401`/`403` auth-rejection error message (actionable — "rejected the api key…") + the
      `Accept: application/json` header (fluxplane sends it; flux didn't). Tested via
      `with_http_status_body`.
- [x] **docker** (33 ops — largest of the batch): schemars-derived (shared `SocketProps.socket`
      flattened); `so()` + `container_create_schema()` deleted; `schema_contract` test. Ported
      gaps: `system.df` `types` filter, `container.top` `args` array, `container.restart`
      `signal`, `container.create`/`run` `mounts`/`open_stdin`/port `protocol`, `network.create`
      `scope`/`ingress`/`enable_ipv4`/`enable_ipv6`, `network.list`/`volume.list`/`image.pull`
      `limit`. Residual fluxplane-only ops needing streaming/hijack/tar/fs intentionally not
      ported (`container.exec`/`stats`/`copy_*`, `image.push`/`build`, `system.prune`, `events`,
      `context.*`).
- [x] All 3 in `MIGRATED_PLUGINS`; guard green.
- [x] Failing-first MockHost tests per ported gap; dev loop green (huggingface 19, opsgenie 11,
      docker 42) + full plugin workspace build/clippy/fmt.

## Notes
- `endpoint_ref`/Docker-daemon architectural splits (do-not-port) apply to all 3.
- docker's residual streaming/hijack ops are a known scope boundary (flux's host `conn.*` +
  blob model doesn't cleanly carry them) — flagged for a future pass, not a regression.
