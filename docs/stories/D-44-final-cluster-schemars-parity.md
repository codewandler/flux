---
id: D-44
title: final cluster schemars migration + parity ports (websearch/jira/confluence/kubernetes/aws) — D-36 COMPLETE
pillar: Agent
status: done
priority:
epic:
design:
note: the final batch — all 5 remaining plugins migrated in parallel; D-36 (schemars migration of all 17 in-repo plugins) is now COMPLETE
---

# final cluster schemars migration + parity ports — D-36 complete

## Goal

Full D-36 per-plugin loop for the final 5 plugins (websearch, jira, confluence, kubernetes, aws),
run in parallel. This completes the schemars migration of all 17 in-repo plugins.

## Acceptance
- [x] **websearch** (2 ops): schemars-derived; inline schema deleted; `schema_contract`. Ported:
      `limit` alias, `queries` array (≤5, ≤500 chars), default max raised to 10 (cap 20, fluxplane
      `NormalizeMax`).
- [x] **jira** (21 ops): schemars-derived; `key_schema()` deleted; `schema_contract`. Ported:
      `body_format` (markdown/adf/both) with ADF→Markdown rendering on `issue.search`/`show`/
      `comment.list`; `issue.search` `fields` override; raw `fields`/`update` maps on
      `issue.create`/`edit`; `attachment.add` `content_bytes` (base64 inline).
- [x] **confluence** (15 ops): schemars-derived; inline schemas deleted; `schema_contract`.
      Ported: `attachment.add` `content_bytes`; `page.list`/`comment.list` pagination tokens
      (`next_start`/`has_more`); JSON error-message extraction. (`index.build` multi-page iteration
      deferred — non-trivial paging loop.)
- [x] **kubernetes** (24 ops): schemars-derived; inline helpers (`s_context`/`s_namespace`/...)
      deleted; `schema_contract` (+ a `op_spec_typed` helper for the one op that needed it).
      Ported: `query`/`limit` on inventory list ops, `pod.logs` `until` bound,
      `deployment.scale` `previous_replicas`, `deployment.restart` `restarted_at`,
      `portforward.start` `duration_seconds`/`expires_at` (default 3600, cap 28800).
- [x] **aws** (11 ops): schemars-derived; `s_region()` deleted; `schema_contract`. Ported:
      `logs.tail`/`logs.groups` integer→RFC3339 timestamp formatting; `aws.test` `latency_ms`.
      (`aws.inspect` `profile`/`profile_env`/`region_env` deferred — host-kit exposes no
      `EnvLookup` capability; the env-cleared subprocess can't read them.)
- [x] All 5 in `MIGRATED_PLUGINS`; guard green — **all 17 in-repo plugins now migrated**.
- [x] Failing-first MockHost tests per ported gap; dev loop green (websearch 8, jira 34,
      confluence 38, kubernetes 42, aws 22) + full plugin workspace build/clippy/fmt.

## Notes
- `endpoint_ref` architectural split (do-not-port) applies to all 5; kubernetes also keeps the
  kubectl-subprocess model (vs fluxplane client-go) + a plugin-local portforward registry (host
  has no `process.list`).
- A few residual gaps (confluence `index.build` paging, aws `inspect` env lookup, jira ADF image
  upload rewriting) flagged for future passes — host-capability-constrained or large enough to
  warrant their own story.