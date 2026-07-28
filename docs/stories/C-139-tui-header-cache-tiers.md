---
id: C-139
title: TUI header cache figure — split the tiers and stop summing last rounds
pillar: Core
status: done
epic: llm-cache-review
design: docs/designs/llm-cache-review.md
note: "header shows `cache {read+write}` as ONE number summed from TurnEnded.usage — so a 12-round turn contributes round 12 only, and a session reading 3.2M from cache renders identically to one writing 3.2M into it; the per-call data is already persisted and already collected at lib.rs:2270, then ignored"
---

# TUI header cache figure — split the tiers and stop summing last rounds

## Goal
Make the header's cache segment mean something: the tiers separated, and the total counted from
every model call rather than the last round of each turn. Serves Core: this is the number a user
looks at all day, and today it is both under-counted and ambiguous.

## Acceptance
- [ ] Failing-first test in `crates/flux-tui` proving the under-count: a session of one turn with
      three model calls (cache_read 90k/60k/20k) currently reports 20k in the header; after the fix
      it reports the sum of all three calls.
- [ ] Read and write are no longer added together. The segment distinguishes them — e.g.
      `cache ↺74k ✎12k` or a hit-% plus split — so a well-cached session is visually distinct from
      one re-writing the prefix every round. Exact glyphs/labels are the implementer's call; the
      test asserts the two tiers are separately recoverable from the rendered line.
- [ ] The header is fed from per-call usage. `EventKind::CallUsage` is already emitted per model
      call (`crates/flux-flow/src/engine.rs:1282`) and the TUI's replay path already collects it
      into `call_usage` (`crates/flux-tui/src/lib.rs:2270`) before discarding it in favour of
      `turn_usage` (`lib.rs:2301-2306`). Use it in both the live path (`record_usage`, `lib.rs:997`)
      and the replay path, so a resumed session's header matches a live one's.
- [ ] Live and replayed headers agree for the same session. Test: build a session, read the live
      header, replay it, assert the same figures.
- [ ] `Usage::accumulate` is untouched — the `ctx` figure keeps its context-window-occupancy meaning
      (`crates/flux-core/src/stream.rs:70-93`). This story changes what the header *reads*, not what
      the accumulator *means*.
- [ ] C-102's narrow-width shedding still holds: the cache segment stays a droppable segment and the
      bar still sheds cost first, then cache, keeping the token total longest
      (`lib.rs:1911-1914`). Test at a narrow width.
- [ ] The same fix is applied to the plain-CLI turn annotation
      (`crates/flux-cli/src/rendering.rs:817`) or a follow-up is filed — the two surfaces should not
      disagree. State which in Progress.
- [ ] Standard gate green (build, test, clippy `-D warnings`, fmt, `flux-codegate`).

## Progress
- DONE 2026-07-28. TUI header now reads per-call `model.call` observations instead of
  `TurnEnded.usage`, and renders `cache <hit>% ↺<read> ✎<write>`; `↑` became total prompt tokens so
  the percentage has a visible denominator.
- The plain-CLI annotation got the same treatment (`usage_annotation_with_cache`), so the two
  surfaces agree. First live run exposed a flaw in the naive split — `ctx 26.1k` beside
  `cache write 44.4k` reads as a contradiction, since ctx is the last round's occupancy while the
  tiers are summed — so the tiers were folded into one self-describing segment.
- No `AgentSink` change was needed: the engine already emits per-call usage as a `model.call`
  observation, and the TUI replay path was already collecting it and discarding it.

## Notes
- Measured 2026-07-28: `flux usage` reports 32% cached across 813 calls, and it is *correct* — it
  reads per-call `CallUsage` (`crates/flux-cli/src/usage.rs:767-793`). The header disagreeing with
  `flux usage` for the same sessions is the bug this story closes; use that as the manual check.
- The C-06 comment at `lib.rs:1907-1909` explains why both tiers were combined in the first place
  (before it, cache was ignored entirely and a cached session looked identical to an uncached one).
  Splitting them is the next step of the same argument, not a reversal — keep the comment's intent
  and extend it.
- Cheap and self-contained; it does not depend on C-133 and can land in parallel.
