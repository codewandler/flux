---
id: C-154
title: Encode risk in the approval sheet's border and title
pillar: Core
status: done
epic: tui-polish-round-2
design:
note: "the sheet already previews the hunk diff (C-115) and supports deny-with-reason (C-113), but border and title are t.accent_style() regardless of whether the pending call writes, deletes, or only reads (rendering.rs:428-432)"
---

# Encode risk in the approval sheet's border and title

## Goal
The approval sheet is the TUI's highest-stakes surface and its most information-rich one — subjects,
a windowed hunk diff, explicit key hints, deny-with-reason (`rendering.rs:316-433`). What it does
not encode is *how dangerous the call is*: border and title are `accent` for every tool
(`rendering.rs:428-432`), so a destructive delete and a read look identical at a glance. Tint the
sheet's border and title by the pending call's effect/risk tier.

## Acceptance
- [ ] The sheet's border and title style are derived from the pending call's risk/effect tier
      (read vs write vs destructive), not a fixed accent — failing-first TestBackend test asserting
      a different border style for a destructive versus a write approval.
- [ ] The tier is taken from data the approval view already carries or can be given without
      widening the approval contract; no new approval decision path, and the y/a/n/d key contract
      (C-103/C-113) is unchanged.
- [ ] MONO/`NO_COLOR` still distinguishes the tiers (modifier or title text, not color alone).

## Progress
- 2026-07-29: Implemented. `ApprovalRequest` (`flux-tui/src/controller.rs`) gained a `pub mutating:
  bool` field alongside the existing `destructive: bool` — together they give the sheet a 3-tier
  read/write/destructive signal without inventing a new classification: `ChannelApprover::request`
  (the per-op path) now reads `intents.is_destructive()`/`is_mutating()` from the `IntentSet` param
  it already received but had discarded (was bound `_intents`, unused — meaning a per-op destructive
  call never disclosed as destructive in the TUI before this fix, only whole-plan approvals did);
  `ChannelApprover::request_plan` now also forwards `plan.mutating` (was already forwarding
  `plan.destructive`). No approval decision path changed — `Approver`/`ApprovalChoice`/the y/a/n/d
  key contract (`approval_key` in controller.rs) are untouched.
  `rendering.rs` adds a private `approval_tier_style(&ApprovalRequest, &Theme) -> (Style, &'static
  str)` used for both the sheet's `Block::bordered().border_style(..)` and its `.title(..)`:
  destructive → `err_style()` + `BOLD` + " approval · destructive "; mutating (non-destructive) →
  `warn_style()` + " approval · write "; else → `accent_style()` + " approval " (unchanged
  read-tier look). Because `Theme::MONO` (`NO_COLOR`) resolves every role to the same
  `Color::Reset`, the BOLD modifier + differing title text (not color) are what separate the tiers
  there, per Acceptance item 3.
  Tests added in `flux-tui/src/lib.rs`: `approval_sheet_border_style_reflects_risk_tier` (TestBackend
  test — reads the sheet's `┌` border corner's `(fg, modifier)` for read/write/destructive requests
  and asserts all three pairwise differ; separately renders a MONO destructive and a MONO write
  sheet and asserts the title text alone distinguishes them); `per_op_request_plumbs_destructive_and
  _mutating_from_intents` (proves the per-op `ChannelApprover::request` fix — a `rm -rf` intent now
  reaches the sheet as `destructive: true, mutating: true`, where before it was always `false/false`
  regardless of the call). `the_approver_raises_a_plan_request_with_its_ops_not_a_bare_count` gained
  an assertion that `plan.mutating` reaches `request.mutating`. Two existing TestBackend fixtures
  (`the_plan_approval_sheet_lists_its_ops_and_targets`, `a_destructive_plan_warns_on_its_own_row`)
  updated with the new field (struct literals without `..Default::default()`).
  Failing-first: verified by reading the pre-change code before editing — the border/title were
  `t.accent_style()` unconditionally and `request()` built `ApprovalRequest` via
  `..ApprovalRequest::default()` ignoring `_intents` entirely, so both new tests provably fail
  against that code (uniform border color across tiers; `destructive`/`mutating` always `false` on
  the per-op path). Did not empirically toggle-revert to re-prove this given the shared tree (five
  other agents editing `flux-tui` concurrently) — reasoning instead from the read-before-edit diff.
  Gate (crate-scoped, flux-tui only): `cargo test -p flux-tui` 148 passed / 0 failed; `cargo clippy
  -p flux-tui --all-targets -- -D warnings` clean; `cargo fmt -p flux-tui -- --check` clean (exit 0).
  Breaking change: `ApprovalRequest` (pub struct in `flux-tui::controller`, re-exported via
  `ApprovalView`) gained the `pub mutating: bool` field — any external construction of
  `ApprovalRequest` without `..Default::default()` needs updating (none exist outside this crate's
  own tests, which were fixed here). Not released yet this cycle per repo convention (SemVer bump
  deferred to the batch cut).
  Non-goal confirmed unchanged: no pending-queue depth was added to the title (per the story's own
  Notes section).

## Notes
- Withdrawn from the original review suggestion: putting a pending-queue depth in the title.
  `ApprovalView` as rendered here holds a single call (`rendering.rs:316`), and no verified
  render-time pending count exists — file separately if that count is ever plumbed.
