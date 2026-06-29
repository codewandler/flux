---
id: D-16
title: Datastore & infra plugin pack (sql, docker, aws)
pillar: Agent
status: backlog
design: docs/designs/fluxplane-plugins-parity.md
---

# Datastore & infra plugin pack

## Goal
Three infra integrations that reach their backends over a socket or a signed API rather than plain bearer
HTTP — the heaviest ports, gated by D-12's ConnDialer (and blob where files move).

## Acceptance (per plugin — match the fluxplane manifest's op set)
- [ ] **sql** (~6, read-only): `test`, `query`, `database.list`, `table.list/show`, `index.list` for
      MySQL/PostgreSQL/SQLite — a Rust driver handed a `host-kit` `Conn` over D-12 `conn.dial`.
- [ ] **docker** (~44): container/image/network/volume/system lifecycle + inspect/prune/exec/logs/stats/
      build/pull/push against the local Docker Engine over the unix socket (D-12 `conn.dial` kind=unix).
- [ ] **aws** (~11, read-only): STS identity, EC2, EKS, RDS, S3 buckets/objects, CloudWatch logs+metrics —
      decide SigV4-over-`http.do` vs the AWS SDK (credentials auth).
- [ ] Read-only / allow-listed by default; destructive docker ops (prune/exec) declared `Risk::Destructive`.
- [ ] Each: full gate green in `plugins/`; smoke entry; `flux plugin skill refresh`.

## Progress
- Backlog. Blocked on **D-12 Slice B** (conn) for sql/docker; aws depends on the SigV4-vs-SDK decision.

## Notes
- Op shapes from `~/projects/fluxplane/fluxplane-plugins/{sql,docker,aws}/manifest.go`; fluxplane reaches these
  via its ConnDialer — flux's D-12 `Host::conn_dial` is the equivalent.
  Epic: [fluxplane-plugins-parity.md](../designs/fluxplane-plugins-parity.md).
