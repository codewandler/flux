---
id: L-38
title: Record canonical plan source — parseable format::format text on PlanAttempted
pillar: Language
status: done
priority:
epic:
design: docs/designs/plan-corpus-and-small-model.md
note: "projection-not-emission: accepted plans become minable as canonical parseable text (plan_source, None-on-overflow, redacted-parseable) while plan_text stays the human render; the hedge that enables flux-native corpus mining without re-opening the L-20 emission decision"
---

# Record canonical plan source — parseable `format::format` text on `PlanAttempted`

## Goal
Every accepted plan lands in events.db with `plan_source` — the canonical, parseable
`flux_lang::format::format` projection of the accepted `DraftAst` — alongside the existing
display-only `plan_text` (`render_pretty`). This makes plan history minable as text (training
corpus, forensics, back-fill cross-check vs the `flow.plan` observation's `plan_ast`) without
changing the planner's emission surface (L-20's keep-json decision stands; the `FLUX_EMISSION`
scaffold stays untouched).

## Acceptance
- [x] `EventKind::PlanAttempted` (`crates/flux-events/src/kind.rs`) gains
      `#[serde(default, skip_serializing_if = "Option::is_none")] plan_source: Option<String>`;
      `PlanAttempt` projection + fold (`projection.rs`) and `record_plan_attempt` (`store.rs`)
      thread it through. Old rows decode as `None` (the `phase`-field precedent, asserted:
      `pre_l38_plan_attempted_rows_decode_without_plan_source`).
- [x] Accepted arm in `crates/flux-flow/src/loop_host.rs` populates it via
      `cap_plan_source(flux_lang::format::format(&c.ast))` where `cap_plan_source` returns
      **`None` when over `PLAN_SOURCE_CAP` (32k) instead of truncating** — invariant: a present
      `plan_source` always parses. Non-accepted outcomes carry `None`.
- [x] `plan_source` is redacted through the same C-22 `Redactor` as `plan_text`/`error`
      (redaction replaces substrings inside string literals — output stays parseable, asserted:
      `redacted_plan_source_still_parses`).
- [x] Failing-first tests: (a) events fold preserves `plan_source` (extended
      `turns` fold test) + old-row decode; (b) end-to-end
      `accepted_plan_records_canonical_parseable_plan_source`
      (`crates/flux-sdk/tests/plan_source.rs`, RED first) asserts
      `flux_lang::parse::parse(&plan_source).unwrap() == accepted ast` (pins the L-18 roundtrip
      at the event boundary); (c) `oversized_plan_source_is_dropped_not_truncated` →
      `plan_source == None` while `plan_text` is still present (truncated as today).
- [x] Full gate green in BOTH workspaces (`cargo test`, `clippy -D warnings`, `cargo fmt
      --check` in root + plugins/). Run 2026-07-04 once the concurrent session's tree compiled
      again: workspace tests all green, fmt clean in both workspaces, clippy clean except ONE
      `unused variable: step_reject_snapshot` warning in the sibling session's own uncommitted
      in-flight test code (not this story's; their pre-commit gate owns it).

## Progress
- 2026-07-04 — filed with the design (approved plan, projection-not-emission decision).
  Implementation deliberately held: a concurrent session has large uncommitted changes in
  `crates/flux-events/src/{kind,projection,store}.rs` (stream-resilience epic); start only after
  those files go quiet, then re-read them fresh before editing.
- 2026-07-04 (later) — **IMPLEMENTED** during a quiet window, RED→GREEN:
  - flux-events: field + fold + store pass-through + back-compat decode test — 41 tests green.
  - flux-flow loop_host: `PLAN_SOURCE_CAP`/`cap_plan_source` (None-on-overflow) on the accepted
    arm + redaction alongside `plan_text`; unit tests for the cap and redacted-parseable —
    207 tests green.
  - flux-sdk: NEW `tests/plan_source.rs` (assembles a real engine via `AgentSpec::assemble`
    around a scripted mock provider + caller-owned in-memory `EventStore`) — roundtrip test was
    RED before the loop_host wiring, GREEN after; oversized test green.
  - Package-scoped gate green (`cargo test`/`clippy`/`fmt` for flux-events, flux-flow, flux-sdk).
  - **Full-workspace gate BLOCKED on the concurrent session**: `flux-providers` currently fails
    to compile from THEIR in-flight A-33 stream-resilience edits (`Chunk::StreamDiagnostic` not
    yet landed in flux-core) — unrelated to this story. Run the full both-workspace gate once
    the tree settles, then close. Beware: the two sessions' cargo processes race on `target/`
    fingerprints (one stale-rmeta false failure observed — re-run after `touch` if it recurs).

## Notes
- Write site: the accepted `TurnOutput::Plan(c)` arm (`loop_host.rs` ~1024) — the only
  `record_plan_attempt` call that carries plan text; compile_error/chat/rejected arms stay `None`.
- The full AST already persists as JSON on the `flow.plan` observation (`loop_host.rs` ~1309,
  `plan_ast`) — history is back-fillable offline via `format`; this story makes the lifecycle
  record self-contained going forward.
- Do NOT reuse `cap_plan_text` (its truncation suffix would poison mining) — new helper.
