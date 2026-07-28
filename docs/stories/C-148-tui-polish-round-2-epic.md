---
id: C-148
title: "TUI polish round 2 — legibility, discoverability, and one overlay language (epic)"
pillar: Core
status: done
epic: tui-polish-round-2
design:
note: "EPIC — the C-101…C-116 wave made the TUI dense and capable; this wave makes it legible and discoverable: a transcript gutter rail, more themes, one shared overlay chrome, fuzzy pickers, risk-colored approval, and an honest empty/quit state"
---

# TUI polish round 2 — legibility, discoverability, and one overlay language (epic)

## Goal
The first TUI wave (C-101…C-116, plus A-65) delivered the capabilities: dense borderless transcript,
themes, approval sheet with diffs, focus/yank, search, help overlay, live tool cards. A read of
`crates/flux-tui/src/{lib,rendering,theme,state}.rs` shows the residual is not capability but
*presentation*: every transcript entry is flush-left plain text, the three list overlays each
hand-roll their own chrome, the pickers can't filter, the approval sheet encodes no risk, and two
states (empty transcript, blank-line Ctrl-C) are silent. This epic funds ten bounded stories that
raise appeal and everyday usability without adding a new capability surface.

## Acceptance
- [ ] C-149…C-158 are filed on the board, each with a failing-first test (the TestBackend +
      `transcript_lines` patterns already used by C-104/C-111/C-115 are the vehicle).
- [ ] No story changes what the TUI *can* do — the envelope, the approval contract, and the
      event/entry pipeline are untouched except where a story names them explicitly (C-158).
- [ ] Existing pinned contracts stay green, in particular the C-109 running-badge row pairing
      (`lib.rs:1554-1567`) and the C-111 focus repaint (`lib.rs:1511-1517`).
- [ ] Epic closes when all ten stories are done or explicitly retired with a recorded reason.

## Progress
- 2026-07-28 epic filed from a TUI UI/UX review pass over `crates/flux-tui/src`.
- 2026-07-28 C-151 and C-153 shipped early in v0.32.0 (one fuzzy ranker behind every picker,
  relative session ages) — the pair the epic's own tiering put first.
- 2026-07-29 **epic CLOSED.** C-149, C-150, C-152, C-154, C-155, C-156 and C-157 landed together
  in v0.33.0, so nine of the ten stories are delivered. The epic's acceptance held: no story
  changed what the TUI *can* do, and both pinned contracts (the C-109 running-badge row pairing,
  the C-111 focus repaint) stayed green — C-149 in particular needed no change to either, because
  the rail is an ordinary leading span by the time layout sees it. One story exceeded its own
  premise: C-154 set out to tint a border and uncovered that the per-op approval path received an
  `IntentSet` and discarded it, so single-op destructive calls had never disclosed as destructive.
- 2026-07-29 **C-158 closes the epic as blocked, per this epic's "done or explicitly retired with
  a recorded reason" clause.** It is not deferred for cost: the investigation established that no
  in-flight tool *content* is observable from the TUI/CLI/runtime crates at all, and that the one
  live relay which does reach a running op withholds content deliberately as a security boundary.
  Streaming it is a boundary decision about where redacted progress may cross, not a rendering
  story — the reasoning and the confirmed installation seam are recorded on C-158 for whoever
  files that decision.

## Notes
- Tiering from the review (highest value first): tier 1 = C-149, C-150, C-151, C-152, C-153;
  tier 2 = C-154, C-155, C-156, C-157; tier 3 (real cost, defer) = C-158.
- Prior art: C-101 (splash/spinners), C-103/C-113/C-115 (approval sheet), C-104 (themes),
  C-109 (live tool cards), C-110 (help overlay), C-111 (focus + yank), C-112 (`@` completion),
  design docs `docs/designs/tui-polish.md` and `docs/designs/tui-makeover.md`.
- Three review claims were corrected against the source before filing and are recorded in the
  stories: per-card expansion already exists (C-155 is discoverability only), the running badge is
  already animated (C-158 is about content, not motion), and Ctrl-C is not a three-step ladder
  (C-156 targets the unconfirmed quit on a blank line).
