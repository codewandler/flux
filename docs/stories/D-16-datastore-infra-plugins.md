---
id: D-16
title: Datastore & infra plugin pack (sql, docker, aws)
pillar: Agent
status: done
design: docs/designs/fluxplane-plugins-parity.md
---

# Datastore & infra plugin pack

## Goal
Three infra integrations that reach their backends over a socket or a signed API rather than plain bearer
HTTP — the heaviest ports, gated by D-12's ConnDialer (and blob where files move).

## Acceptance (per plugin — match the fluxplane manifest's op set)
- [x] **sql** (6, read-only): `test`, `query`, `database.list`, `table.list/show`, `index.list` for
      PostgreSQL over `host-kit::ConnStream`; MySQL and SQLite return clear unsupported errors.
- [x] **docker** (33 core ops): container/image/network/volume/system lifecycle over the local Docker Engine
      REST API via the guarded Unix socket.
- [x] **aws** (11, read-only): STS identity, EC2, EKS, RDS, S3 buckets/objects, CloudWatch logs+metrics via
      the host-managed `aws` CLI path.
- [x] Read-only / allow-listed by default; destructive docker lifecycle/prune ops declared high risk.
- [x] Each: full gate green in `plugins/`; smoke entry; `flux plugin skill refresh`.

## Progress
- **Done (2026-06-30).** Added `host-kit::ConnStream` and MockHost conn-response scripting, then ported
  `sql`, `docker`, and `aws`. Full `plugins/` workspace test/clippy gate is green.

## Residuals
- Docker streaming/hijack operations (`exec`, `stats`, log follow, image build/push, events) are not shipped;
  they need a later long-lived stream/hijack design.
- SQL live Postgres interop still needs an env-gated smoke against a real server. The current tests replay
  hand-crafted Postgres frames and validate SCRAM against the RFC vector.

## Notes
- Op shapes from `~/projects/fluxplane/fluxplane-plugins/{sql,docker,aws}/manifest.go`; fluxplane reaches these
  via its ConnDialer — flux's D-12 `Host::conn_dial` is the equivalent.
  Epic: [fluxplane-plugins-parity.md](../designs/fluxplane-plugins-parity.md).
