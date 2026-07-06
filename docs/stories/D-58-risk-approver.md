---
id: D-58
title: RiskApprover — risk-tier confirm gate beside Allow/Deny
pillar: Agent
status: done
epic: consumer-gaps
note: "from the 2026-07-06 downstream-consumer review: flux ships only AllowApprover/DenyApprover though the injection seams exist; the consumer had to hand-roll the obvious middle policy (gate writes by risk tier behind an explicit-consent marker)"
---

# RiskApprover

## Goal
Ship the obvious middle ground between `AllowApprover` (everything) and `DenyApprover` (nothing): an
`Approver` that permits reads freely and gates **write-effect** tools by their declared **risk
tier**, requiring an explicit consent marker in the permission subjects for writes at/above a
configurable threshold. Any non-headless flux consumer needs exactly this policy.

## Why (evidence)
flux-runtime provides only `AllowApprover`/`DenyApprover` (`crates/flux-runtime/src/lib.rs:675-695`);
flux-orchestrate's `SubAgentApprover` is destructive-deny only. The injection seams exist
(`FlowClientBuilder::approver`, `SubAgents::with_approver`) — flux provides the socket but no
risk-tier plug. The reviewed downstream consumer hand-rolled it: an `Approver` that snapshots
`(writes, risk)` per tool from its registry's `ToolSpec`s, gates `Effect::Write` by `Risk` tier,
passes unknown tool names through, uses a consent-subject marker for explicit user confirmation, and
fails closed on the plan gate. Only its marker constant and env override are app-specific.

## Acceptance
- [x] `RiskApprover` in flux-runtime (own module, e.g. `approval.rs`): constructed from a
      `ToolRegistry` snapshot of each tool's `(has write effect, risk tier)`; behavior — reads
      auto-approved; writes below the threshold auto-approved; writes at/above the threshold
      approved only when the call's permission subjects carry the configured consent marker;
      unknown tool names pass through (they never entered the snapshot — the envelope still guards
      them); plan-level gate fails closed.
- [x] Consent marker + risk threshold are constructor parameters (with sensible defaults, e.g.
      `"user-confirmed"` + gate `High` and above) — no env reads inside the type.
- [x] Failing-first tests: read passes; low-risk write passes; high-risk write without marker
      denied; with marker approved; unknown tool passes; plan gate fail-closed; snapshot reflects
      registry at construction (a tool added later = unknown).
- [x] Doc comment places it in the ladder (Allow < Risk < Deny) and states what it does NOT do
      (it is an approval policy, not the authorization envelope).
- [x] Full gate green; consumer-compat `cargo check` clean (additive).

## Progress
- 2026-07-06 filed from the consumer review.
- 2026-07-07 implemented: `RiskApprover` added in `crates/flux-runtime/src/approval.rs` (new
  module, wired via `mod approval; pub use approval::{RiskApprover, DEFAULT_CONSENT_MARKER};` in
  `lib.rs` next to the existing `perm` module wiring). Constructor `RiskApprover::new(&ToolRegistry)`
  snapshots `(has write effect, risk tier)` per tool at construction; defaults are the consent
  marker `"user-confirmed"` (`DEFAULT_CONSENT_MARKER`) and threshold `Risk::High` (gates `High` and
  `Destructive`); builder-style `.with_marker(..)` / `.with_threshold(..)` override either, no env
  reads in the type. `request_plan` is overridden to fail closed on any gated write named in the
  plan (mirrors the default per-op policy, closed instead of open). Wrote 8 failing-first tests
  covering the full acceptance matrix plus marker/threshold overrides; verified failing-first by
  temporarily breaking the threshold check (`gates()` ignoring risk) and confirming exactly the two
  threshold-dependent tests failed for the right reason, then restored the correct logic. Full gate
  green: `cargo build --workspace`, `cargo test --workspace` (89/89 test-result blocks ok),
  `cargo clippy --workspace --all-targets -- -D warnings` (clean), `cargo fmt --check` (root +
  `plugins/`, both clean). Consumer-compat `cargo check --workspace` in the downstream consumer's
  repo stays clean (additive-only change, untouched). Additive only — no existing files besides
  `lib.rs`'s two new wiring lines were touched.

## Notes
- Adoption story in the consumer's repo follows: replace its hand-rolled gate with this type,
  parameterized by its own marker/threshold.
