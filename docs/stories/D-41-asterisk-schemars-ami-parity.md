---
id: D-41
title: asterisk schemars migration + AMI parity ports (originate fields, peer comment, ping duration)
pillar: Agent
status: done
priority:
epic:
design:
note: full D-36 per-plugin loop for asterisk — schemars migration (8 ops, shared AMIConn via flatten) + fluxplane parity re-audit + ported originate fields / peer comment / ping duration / timeout
---

# asterisk schemars migration + AMI parity ports

## Goal

Full D-36 per-plugin loop for the `asterisk` AMI plugin: schemars migration of all 8 ops, a
fluxplane parity re-audit, and porting the gaps surfaced.

## Acceptance
- [x] All 8 asterisk ops schemars-derived via `read_op_typed::<T>`/`write_op_typed::<T>`; the local
      `so()` helper deleted. `Risk::Destructive`/`Risk::High` preserved on the write ops. Handlers
      unchanged in extraction style (schema-only structs; `flex_str`/`flex_i64`/`flex_bool` stays,
      D-34 precedent).
- [x] Shared `AMIConn` struct (the portable part of fluxplane's `AMIActionInput` — `timeout`)
      embedded via `#[serde(flatten)]`/`#[schemars(flatten)]`.
- [x] `schema_contract` test encodes the post-migration contract; `asterisk` in `MIGRATED_PLUGINS`;
      guard green.
- [x] **Parity re-audit** vs `~/projects/fluxplane/fluxplane-plugins/asterisk/`. Core shape/semantics
      matched 1:1 across the 8 ops. Architectural split (do-not-port): fluxplane's per-call
      `endpoint_ref`/`URL`/`credential_ref` in `AMITargetInput` — flux resolves the AMI host via the
      manifest `asterisk.ami` endpoint + `host.endpoint(...)`.
- [x] **Ported gaps:**
      - `timeout` (string, Go-duration) on every op — parsed/validated (same host-limitation as sql:
        `conn.*` exposes no per-call timeout, so validated not enforced).
      - `call.originate` missing params `early_media`/`channel_id`/`other_channel_id` — handler now
        sends `EarlyMedia`/`ChannelId`/`OtherChannelId` AMI fields.
      - `peer.list` output `comment` — PJSIP from `ActiveChannels` ("N active channel(s)"); SIP/IAX
        from `Description`.
      - `ami.ping` output `duration_ms`.
- [x] Failing-first MockHost tests per change; dev loop green (12 asterisk tests).

## Notes
- A `last_call` queue-member output field was intentionally NOT ported — it needs reliable RFC3339
  UTC date rendering and the crate has no `chrono`/`time` dependency; adding an ad-hoc calendar
  converter would be worse than leaving the gap explicit. Reported honestly.
- Same host-timeout limitation as sql (D-40): `Host::conn_dial`/`conn_dial_ref` + `dial_scoped`
  expose no per-call timeout, so `timeout` is parsed/validated but not wire-enforced.
