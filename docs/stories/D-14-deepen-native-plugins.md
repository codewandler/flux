---
id: D-14
title: Deepen the 8 native plugins to full op-parity
pillar: Agent
status: done
design: docs/designs/fluxplane-plugins-parity.md
note: "all 8 `plugins/` at fluxplane op + **behavioural** parity (+~160 ops): gitlab 6→64, slack 5→30, kubernetes 5→24, jira 3→21, confluence 3→15, prometheus 4→8, loki 3→5, websearch +`provider.list`. Added two **host protocol** capabilities (managed background processes `process.spawn/read/status/kill`; binary HTTP body `body_b64`/`response_binary`). jira/confluence auth re-ported to the reference (Bearer/`cloud_id` gateway + Basic fallback); k8s port-forward on managed processes; byte-exact attachments/files; jira ADF + transition scorer, slack mentions/unreads, gitlab `diff.lines` regex ported faithfully. One MockHost test per op; `plugins/` + host gate green"
---

# Deepen the 8 native plugins to full op-parity

## Goal
Bring the 8 plugins shipped under D-08 up to their fluxplane counterparts' full operation set. flux currently
exposes a fraction; this closes the depth gap so each integration is actually usable for real DevOps work.
**Epic** — ship per plugin.

## Acceptance (per-plugin slice — match the fluxplane manifest's op set)
- [x] **gitlab** 6 → **64**: MR review/diff/discussion/approve/merge, jobs, CI variables, branches, releases,
      repository files/commits/tags/tree, snippets, compare, search, `repository.archive` (blob), `index.build`.
- [x] **slack** 5 → **30**: edit/delete, files (upload/download/info/list/delete, blob), bookmarks, reactions,
      emoji, presence, unreads, mentions, `index.build`.
- [x] **jira** 3 → **21**: create/edit/delete/transition, comments, attachments (blob), create_meta, user.search,
      `index.build` — hand-rolled base64 dropped (D-12 `AuthMethod::basic`).
- [x] **confluence** 3 → **15**: page create/update/delete, comments, attachments (blob), user.search,
      `index.build` — hand-rolled base64 dropped (D-12 `AuthMethod::basic`).
- [x] **kubernetes** 5 → **24**: node/service/ingress/container list+show, deployment scale/restart/history,
      secret.read, port-forward start/stop/list, endpoint.discover — **kubectl-CLI** (decision below).
- [x] **loki** 3 → **5** (`test`, `recent_logs`, `metric`; non-Bearer Basic + `X-Scope-OrgID` tenant header);
      **prometheus** 4 → **8** (`series`, `targets`, `rules`, `alerts`); **websearch** confirmed (added
      `provider.list` + a provider selector).
- [x] Each plugin: full gate green in `plugins/`; one MockHost test per op; smoke entries updated. (No new
      workspace dependency.)

## Progress
- **Done (2026-06-30).** Two passes, one sub-agent per plugin in parallel each time, full gate green:
  1. *Op-coverage* — all 8 plugins to fluxplane op-count parity (+~160 ops); `index.build` on the HTTP plugins.
  2. *Fidelity correction* — fixed the unauthorized divergences the first pass introduced, host-first:
     - **Host extensions** (approved): a **managed background-process** capability (`process.spawn`/`read`/
       `status`/`kill`, a per-session registry beside `conns`/`blobs` in `SystemHostCaps`, gated by the
       manifest `process` allow-list, backed by `flux_system::System::spawn_background`) + a **binary HTTP
       body** channel (`body_b64` request, `response_binary` response) — so uploads/downloads are byte-exact.
       In `flux-system` + `flux-plugin` + host-kit.
     - **jira/confluence auth** re-ported to the reference: **Bearer `api_token` via the `cloud_id` gateway**
       primary, **Basic (email:token) retained as a configurable fallback**, selected per request from the
       configured env; the host injects both schemes (no in-plugin base64).
     - **kubernetes** port-forward start/stop/list re-implemented on the managed-process capability (the
       host spawns/holds `kubectl port-forward`); `pod.exec` confirmed one-shot per the reference.
     - **Per-plugin fidelity** matched to the reference: jira markdown→ADF renderer + transition scorer;
       slack `mentions` replied/acked/pending classification + `unreads` `last_read` cursor math + byte-exact
       files; gitlab `mr.diff.lines` regex (+ `mr.merge` `auto_merge`, `pipeline.create` variable
       validation); loki/prometheus datasource shapes + auth-purpose names aligned (prometheus rejects empty
       `query`); websearch confirmed faithful.

## Residual divergences — closed (2026-06-30, deps authorized)
- **confluence storage↔markdown** — ✅ full conversion ported from the reference (`pulldown-cmark` +
  `quick-xml`): macros, tables, nested/task lists, `ac:link`/`ac:image`, emoticons, layouts.
- **loki** — ✅ entry ids now SHA1 over the reference's exact input (sorted-key JSON labels + raw-ns ts +
  line; verified vs Go vectors) and timestamps RFC3339Nano (`sha1` + `time` deps).
- **prometheus** — ✅ `query`/`query_range` return the reference's typed `{result_type, samples, series,
  count, truncated}` (200-series/500-point caps) and `query_range` takes `since`/`until`/`step` defaults.
- **slack** — ✅ `mentions`/`unreads` gained `since`/`unhandled`/`tickets` (+ aggregated ticket records);
  `role` (a token-role system) and mrkdwn rendering (Thread-only) confirmed **N/A** for these two ops.

### Minor remaining (accepted — low value)
- **prometheus** `labels`/`targets`/`alerts`/`series`/`rules` still return Prometheus's (already structured)
  raw JSON rather than the reference's per-op typed structs; record ids use a positional fallback vs the
  reference's sha1.
- **loki.metric** emits a numeric unix-seconds `timestamp` (the reference's metric path uses second-precision
  RFC3339) — a different shape, left to avoid churning the metric result.
- **jira** browse-link decoration (`web_url` via the `accessible-resources` endpoint) omitted (output cosmetic).

## Notes
- Op shapes (copy, not code) from `~/projects/fluxplane/fluxplane-plugins/<plugin>/manifest.go`. Pattern:
  `plugins/gitlab/src/main.rs`. Epic: [fluxplane-plugins-parity.md](../designs/fluxplane-plugins-parity.md).
