---
id: D-15
title: Observability & AI plugin pack (alertmanager, grafana, opsgenie, huggingface)
pillar: Agent
status: backlog
design: docs/designs/fluxplane-plugins-parity.md
---

# Observability & AI plugin pack

## Goal
Four missing **HTTP** integrations that round out the monitoring/incident surface plus the HF hub. All are
plain HTTP plugins on `host-kit` — they need only D-12's non-Bearer auth injection, no ConnDialer.

## Acceptance (per plugin — match the fluxplane manifest's op set)
- [ ] **alertmanager** (~5): `test`, `alerts`, `silence.list/create/delete` (optional HTTP basic via D-12).
- [ ] **grafana** (~20): datasource list/health, dashboards, folders, annotations, alerts+silences, and the
      Loki/Prometheus/Alertmanager/Tempo proxy queries (`config` auth via D-12 header).
- [ ] **opsgenie** (~8): `test`, `alert.list/get/ack/close/note`, `oncall`, `schedule.list` (`GenieKey`
      header auth via D-12 `Header` scheme).
- [ ] **huggingface** (~9): `whoami`, model/dataset/space search+get, `chat`, `embed`, `test` (bearer).
- [ ] Contribute `flux-datasource` records where natural (alerts, dashboards, models) so they feed the index.
- [ ] Each: full gate green in `plugins/`; smoke entry; `flux plugin skill refresh`.

## Progress
- Backlog. Blocked on **D-12 Slice A** (auth). huggingface (bearer) could land before D-12.

## Notes
- Op shapes from `~/projects/fluxplane/fluxplane-plugins/{alertmanager,grafana,opsgenie,huggingface}/manifest.go`.
  Epic: [fluxplane-plugins-parity.md](../designs/fluxplane-plugins-parity.md).
