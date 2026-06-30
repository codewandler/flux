---
id: D-37
title: Port homer call.analyze multi-leg correlation logic from fluxplane (parity)
pillar: Agent
status: done
priority:
epic:
design:
note: the pilot "port the gap" sub-recipe from the D-36 plan — flux homer's call.analyze was a stripped-down stub (seed-by-call_id only); ported the full fluxplane multi-leg correlation analysis (from_user+to_user seed, number fan-out, correlation groups + temporal overlap, number matching, multi-leg report)
---

# Port homer call.analyze multi-leg correlation logic

## Goal

Bring flux `homer.call.analyze` to behavioural parity with the fluxplane reference
(`~/projects/fluxplane/fluxplane-plugins/homer/analyze.go`). The flux op currently only locates a
seed by `call_id` and extracts correlation values from the seed transaction; it does not do the
fan-out / correlation-group / temporal-overlap / multi-leg analysis that defines the op. Port the
full logic + the 5 input params it needs (`from_user`, `to_user`, `numbers`, `headers`, `limit`).

This is the first "port the gap" sub-story under the D-36 plan (`.flux/plans/d36-plugin-schemars-parity-smoke.md`),
proving the sub-recipe before applying it to gitlab/slack/….

## Acceptance

- [x] `CallAnalyzeInput` advertises `from_user`, `to_user`, `numbers`, `headers`, `limit`
      (matching fluxplane's `CallAnalyzeInput` tags), in addition to the existing `call_id` /
      `correlation_header` / `since` / `until`. `render` stays advertised (parity) but is a
      documented deferred gap (SVG renderer not ported — cross-cutting with `call.show`).
- [x] Handler seeds by `call_id` **or** `from_user`+`to_user` (ambiguous-when-many → clear error,
      matching fluxplane's `ambiguous` path).
- [x] Handler fans out by the seed caller + extra `numbers` (±30m margin), merges seed + fan
      results (dedup by record id), fetches the merged transaction.
- [x] Handler extracts the correlation header from each candidate INVITE, groups by correlation
      value, keeps groups that temporally overlap the seed (start within `[-5s, +30s]` of the seed
      start), and additionally keeps fan-out legs involving an extra `numbers` entry.
- [x] Result shape matches fluxplane `CallAnalyzeResult`: `seed_call_id`,
      `correlation_header`, `correlation_values` (sorted), `legs` (each with `call_id`/`seed`/
      `start_time`/`from`/`to`/`status`/`duration`/`correlation`/`headers`/`matched_by`),
      `leg_count`, `events`, `event_count`, `ladder`. (`route` and `ladder_blob` are deferred —
      recorded below.)
- [x] A **failing-first** MockHost test exercises the from_user+to_user seed path + a two-leg
      correlation (a second INVITE sharing the correlation header, temporally overlapping) and
      asserts both legs appear with the right `matched_by`. It fails before the port (the old
      handler errored on missing `call_id`) and passes after.
- [x] `schema_contract` test updated to encode the new `call.analyze` contract.
- [x] Dev loop green: plugin workspace `build`/`test`/`clippy -D warnings`/`fmt`.

## Scope

In: `plugins/homer/src/main.rs` `op_call_analyze` + `CallAnalyzeInput` + tests. Reuses existing
flux helpers (`group_calls`, `number_alternatives`, `build_smart_input`, `flow_events`,
`render_ladder`, `build_transaction_payload`, `extract_sip_header`, `format_duration_ms`,
`clamp_limit`, `ms_to_rfc3339`). Adds one small `merge_records` (dedup-by-id) helper.

Deferred (recorded, separate follow-ups):
- **`render: svg` → `ladder_blob`**: needs porting `ladder_svg.go` (`RenderLadderSVG`), a
  cross-cutting port that also affects `homer.call.show` (where `render` is currently a dead
  param). One story for both.
- **`route` per leg**: needs porting `DeriveRoute`/`FormatRoute` from fluxplane `calls.go`.

## Progress
- (running log)

## Notes
- Fluxplane reference: `~/projects/fluxplane/fluxplane-plugins/homer/analyze.go`.
- Architectural split (Gap A, do NOT port): `endpoint_ref` per-call targeting — flux uses
  `host.endpoint("homer.endpoint")` + `~/.flux/endpoints.toml` (D-29 reference-IO), not the
  fluxplane per-call `EndpointRef`.
