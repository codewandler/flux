---
id: D-14
title: Deepen the 8 native plugins to full op-parity
pillar: Agent
status: backlog
design: docs/designs/fluxplane-plugins-parity.md
---

# Deepen the 8 native plugins to full op-parity

## Goal
Bring the 8 plugins shipped under D-08 up to their fluxplane counterparts' full operation set. flux currently
exposes a fraction; this closes the depth gap so each integration is actually usable for real DevOps work.
**Epic** — ship per plugin.

## Acceptance (per-plugin slice — match the fluxplane manifest's op set)
- [ ] **gitlab** 6 → ~60: MR review/diff/discussion/approve/merge, jobs, CI variables, branches, releases,
      repository files/commits/tags/tree, snippets, compare, search, `index.build`.
- [ ] **slack** 5 → ~30: edit/delete, files (upload/download/info/list/delete), bookmarks, reactions, emoji,
      presence, unreads, mentions, `index.build` (file ops use D-12 blob).
- [ ] **jira** 3 → ~20: create/edit/delete/transition, comments, attachments, create_meta, user.search,
      `index.build` — and drop the hand-rolled base64 (use D-12 `auth_purpose` Basic).
- [ ] **confluence** 3 → ~15: page create/update/delete, comments, attachments, user.search, `index.build`
      — also drop hand-rolled base64.
- [ ] **kubernetes** 5 → ~24: node/service/ingress/container list+show, deployment scale/restart/history,
      secret.read, port-forward start/stop/list, endpoint.discover (decide kubectl-CLI vs native client over
      D-12 conn).
- [ ] **loki** 3 → 5 (`test`, `recent_logs`, `metric`); **prometheus** 4 → 8 (`test`, `series`, `rules`);
      **websearch** — confirm Tavily+DDG coverage vs fluxplane.
- [ ] Each slice: full gate green in `plugins/`; `flux plugin skill refresh`; smoke entry updated.

## Progress
- Backlog. Blocked on D-12 (auth for jira/confluence; blob for file ops; conn for native k8s).

## Notes
- Op shapes (copy, not code) from `~/projects/fluxplane/fluxplane-plugins/<plugin>/manifest.go`. Pattern:
  `plugins/gitlab/src/main.rs`. Epic: [fluxplane-plugins-parity.md](../designs/fluxplane-plugins-parity.md).
