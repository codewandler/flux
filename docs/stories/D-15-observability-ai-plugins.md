---
id: D-15
title: Observability & AI plugin pack (alertmanager, grafana, opsgenie, huggingface)
pillar: Agent
status: done
design: docs/designs/fluxplane-plugins-parity.md
note: added native `alertmanager` (5 ops), `grafana` (20), `opsgenie` (8), and `huggingface` (9), with datasource contributions and env-gated smoke coverage; full `plugins/` gate green
---

# Observability & AI plugin pack

## Goal
Four missing **HTTP** integrations that round out the monitoring/incident surface plus the HF hub. All are
plain HTTP plugins on `host-kit` — they need only D-12's non-Bearer auth injection, no ConnDialer.

## Acceptance (per plugin — match the fluxplane manifest's op set)
- [x] **alertmanager** (5): `test`, `alerts`, `silence.list/create/delete` (optional HTTP basic via D-12).
- [x] **grafana** (20): datasource list/health, dashboards, folders, annotations, alerts+silences, and the
      Loki/Prometheus/Alertmanager/Tempo proxy queries (`config` auth via D-12 header).
- [x] **opsgenie** (8): `test`, `alert.list/get/ack/close/note`, `oncall`, `schedule.list` (`GenieKey`
      header auth via D-12 `Header` scheme).
- [x] **huggingface** (9): `whoami`, model/dataset/space search+get, `chat`, `embed`, `test` (bearer).
- [x] Contribute `flux-datasource` records where natural (alerts, dashboards, models) so they feed the index.
- [x] Each: full gate green in `plugins/`; smoke entry; `flux plugin skill refresh`.

## Progress
- **Done (2026-06-30).** Ported all four plugins as native Rust plugins in `plugins/`: alertmanager 5 ops,
  grafana 20, opsgenie 8, huggingface 9. Package gates passed during the fan-out; the full `plugins/`
  workspace test/clippy gate is green.

## Notes
- Op shapes from `~/projects/fluxplane/fluxplane-plugins/{alertmanager,grafana,opsgenie,huggingface}/manifest.go`.
  Epic: [fluxplane-plugins-parity.md](../designs/fluxplane-plugins-parity.md).
