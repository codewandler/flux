---
id: C-140
title: In-TUI `/usage` overlay — watch the cache work (or collapse) as the turn runs
pillar: Core
status: ready
priority: 3
epic: llm-cache-review
design: docs/designs/llm-cache-review.md
note: "`flux usage` is offline, per-model, and whole-history; nothing shows the CURRENT session per round — which is where a mid-turn cache collapse from tool churn or TTL expiry is actually visible; per-round CallUsage data is already persisted"
---

# In-TUI `/usage` overlay — watch the cache work (or collapse) as the turn runs

## Goal
A live, per-session usage dashboard inside the TUI: this turn's context, hit rate, and
read/write/fresh split, plus a per-round list that makes a mid-turn cache collapse visible as it
happens. Serves Core: `flux usage` answers "how did last fortnight go"; nothing answers "is this
turn caching, and where did it stop".

## Acceptance
- [ ] `/usage` opens a modal overlay in the TUI and `esc` closes it. Registered like the other
      overlays (help overlay, C-110) and listed in help.
- [ ] Approved layout (adjust for theming/width, keep the information):

```
┌ usage · s_1499 ────────────────────────────────┐
│ claude/claude-fable-5 · turn 7 · round 4       │
│                                                │
│ this turn                                      │
│   ctx    ▁▂▃▅▆▇██   128.4k                     │
│   hit    ████████████░░░░░░░░  58%             │
│          read 74.2k · write 12.1k · fresh 42.1k│
│                                                │
│ per round                                      │
│   1  ██████████████████░░  91%                 │
│   2  ████████████░░░░░░░░  61%                 │
│   3  ████████░░░░░░░░░░░░  42%  ← tools churned│
│   4  ███████░░░░░░░░░░░░░  38%                 │
│                                                │
│ session Σ 412.8k ctx · 61% hit · $1.84         │
└─────────────────── esc to close ───────────────┘
```

- [ ] Per-round rows come from per-call usage (`EventKind::CallUsage`), not `TurnEnded.usage`.
      `hit` is `cache_read / (input + cache_read + cache_creation)` for that call; `fresh` is
      `input_tokens`. The three-way read/write/fresh split must sum to ctx — test asserts it.
- [ ] The overlay updates live as rounds complete during a running turn, not only at turn end.
- [ ] The annotation column (`← tools churned` in the mockup) is **derived, not guessed**: it marks a
      round where the advertised tool set differs from the previous round's, which is the signal
      A-95 exists to remove. If that signal is not cheaply available at the TUI layer, drop the
      column rather than approximating it — a wrong causal hint is worse than none. State the
      decision in Progress.
- [ ] Degrades on a narrow/short terminal: bars shrink, the per-round list scrolls or truncates with
      a count, and the overlay never overflows its frame. Test at a small size.
- [ ] Themed through `crates/flux-tui/src/theme.rs` (C-104) — no hardcoded colors; renders
      acceptably in mono.
- [ ] A session with no usage yet (offline `-m mock`, or before the first call) renders an empty
      state rather than a division-by-zero or a bare frame.
- [ ] Standard gate green (build, test, clippy `-D warnings`, fmt, `flux-codegate`).

## Progress
- (not started)

## Notes
- Layout chosen by the user 2026-07-28 from three options (richer `flux usage` render / in-TUI
  overlay / HTML export). The other two remain open as follow-ups if this proves useful.
- Ordering: land C-139 first — it establishes reading per-call usage in the TUI, which this overlay
  then reuses rather than re-deriving.
- Do not duplicate `flux usage`'s aggregation logic. If a shared projection makes sense, it belongs
  in `flux-events` next to the existing `cost_summary` projection
  (`crates/flux-events/src/store/mod.rs:1270`), not copy-pasted into the TUI.
- This overlay is the fastest way to *see* the wave-1/wave-2 fixes work: tool churn (A-95) and TTL
  expiry (C-135) both show up as a step down in the per-round bars, which is exactly what the
  mockup's rounds 3–4 depict.
