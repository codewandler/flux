---
id: D-17
title: Telephony plugin pack (asterisk, homer)
pillar: Agent
status: backlog
design: docs/designs/fluxplane-plugins-parity.md
---

# Telephony plugin pack

## Goal
The two telephony integrations from fluxplane — Asterisk PBX control and Homer SIP capture. These serve the
managed-agents **voice** side (a managed-agents operator inspecting/controlling calls), so they complete parity in
a direction flux otherwise has no coverage for.

## Acceptance (per plugin — match the fluxplane manifest's op set)
- [ ] **asterisk** (~8): `ami.ping`, `command`, `peer.list`, `channel.list/hangup`, `devicestate.list`,
      `queue.status`, `call.originate` over the AMI TCP socket (D-12 `conn.dial` kind=tcp; AMI is line-based).
- [ ] **homer** (~8, HTTP): `test`, `search`, `call.list/show/qos/analyze`, `pcap.export`, `alias.list`
      (`config` auth via D-12 header).
- [ ] `call.hangup`/`call.originate` declared `Risk::Destructive`/write; everything else read-only.
- [ ] Each: full gate green in `plugins/`; smoke entry; `flux plugin skill refresh`.

## Progress
- Backlog. Blocked on **D-12 Slice B** (conn) for asterisk; homer (HTTP) only needs Slice A.

## Notes
- Op shapes from `~/projects/fluxplane/fluxplane-plugins/{asterisk,homer}/manifest.go`. Serves the managed-agents
  voice surface (see the realtime-voice + managed-agents track). Epic:
  [fluxplane-plugins-parity.md](../designs/fluxplane-plugins-parity.md).
