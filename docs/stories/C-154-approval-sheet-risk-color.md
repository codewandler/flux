---
id: C-154
title: Encode risk in the approval sheet's border and title
pillar: Core
status: backlog
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
- (not started)

## Notes
- Withdrawn from the original review suggestion: putting a pending-queue depth in the title.
  `ApprovalView` as rendered here holds a single call (`rendering.rs:316`), and no verified
  render-time pending count exists — file separately if that count is ever plumbed.
