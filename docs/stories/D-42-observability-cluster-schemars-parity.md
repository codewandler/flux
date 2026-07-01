---
id: D-42
title: observability cluster schemars migration + parity ports (grafana/prometheus/loki/alertmanager)
pillar: Agent
status: done
priority:
epic:
design:
note: full D-36 per-plugin loop for the 4 observability plugins — schemars migration + fluxplane parity re-audit + ported gaps (4 workers in parallel); MockHost gained with_http_status_body for error-path tests
---

# observability cluster schemars migration + parity ports

## Goal

Full D-36 per-plugin loop for the 4 observability plugins (grafana, prometheus, loki,
alertmanager), run in parallel. Each: schemars migration of every op, a fluxplane parity
re-audit, and porting the gaps surfaced.

## Acceptance
- [x] **grafana** (21 ops): all schemars-derived via `read_op_typed`/`write_op_typed`; `so()`
      helper deleted; `schema_contract` test locks all 21 contracts.
- [x] **prometheus** (16 ops): schemars-derived; `so()` deleted; `schema_contract` test
      (`#[cfg(test)]`).
- [x] **loki** (9 ops): schemars-derived; `so()` deleted; `schema_contract` test.
- [x] **alertmanager** (7 ops): schemars-derived; `so()`/inline schemas deleted; `schema_contract`
      test.
- [x] All 4 in `MIGRATED_PLUGINS`; guard green.
- [x] Fluxplane parity re-audit done for each (matched / architectural-split `endpoint_ref` /
      ported gaps).
- [x] **MockHost gained `with_http_status_body`** (host-kit test infra) for error-path tests
      (a canned `http.do` response with a custom status code + raw body — e.g. a 503 from a
      readiness endpoint), checked before `http_seq`/`http`. Used by the prometheus ready-check
      test.
- [x] Failing-first MockHost tests per ported gap; dev loop green for all 4 (grafana 21,
      prometheus 16, loki 19, alertmanager 7) + the full plugin workspace build/clippy/fmt.

## Notes
- Each plugin's exact ported gaps are recorded in `DRIFT.md` § D-42 (per-plugin).
- `endpoint_ref` architectural split (do-not-port) applies to all 4: flux resolves
      `<plugin>.endpoint` via `host.endpoint(...)` + reference-IO (D-29).
