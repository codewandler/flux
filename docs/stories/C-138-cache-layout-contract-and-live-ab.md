---
id: C-138
title: Pin the cache-layout contract + close the epic with a live A/B on both providers
pillar: Core
status: done
epic: llm-cache-review
design: docs/designs/llm-cache-review.md
note: "nothing today pins the realized breakpoint layout or fails when a new system segment / tool-set change silently halves the cache — A-23 pins the count only; this story pins the layout, records the live A/B, and documents the invariant so the epic does not have to be repeated"
---

# Pin the cache-layout contract + close the epic with a live A/B on both providers

## Goal
Turn the epic's fixes into a maintained invariant: a regression test that pins where breakpoints
land, recorded live numbers proving the fixes work on both providers, and a documented contract the
next segment or tool-set change has to respect. Serves Core: cache behavior stops being unowned.

## Acceptance
- [ ] A codec-level regression test asserts the **realized layout**, not just the count, for both
      the `anthropic` and `claude` (identity-prefix) segment layouts at the intent and explore
      stages: total `cache_control` keys ≤ 4, the tools+system prefix breakpoint present and on the
      1h TTL, the conversation tail breakpoint present and on the 5m default, and the per-turn
      segment after the last system breakpoint. Extends A-23's `count_cache_control` test rather
      than duplicating it.
- [ ] A test fails loudly if a new `cache: true` segment is added to a layout that is already at the
      union budget — the failure message names the budget and points at
      `docs/designs/llm-cache-review.md`, so the next author is told what to do rather than just
      that a number changed.
- [ ] Live A/B recorded in `docs/designs/llm-cache-review.md`, using the C-133 harness, for:
      `claude/*` (baseline vs. C-134+C-135+A-95) and `codex/*` (baseline vs. C-136+C-137). Each
      entry records provider, model, round count, turn-level hit rate before and after, and the
      date. Also re-run `anthropic/claude-sonnet-5` so the claude-vs-anthropic question from C-133
      has a closing answer.
- [ ] A "cache layout" section added to the provider documentation stating the invariant: *tools and
      the cached system segments form the stable prefix (1h TTL); per-turn text goes after the last
      system breakpoint; the conversation tail carries the rolling breakpoint (5m TTL); the union
      stays ≤ 4.* Placed where a provider author will find it — `docs/architecture.md`'s provider
      section and/or the `crates/flux-providers/src/messages/mod.rs` module doc.
- [ ] The design doc's **Status** moves to `implemented <date>`, its story links are all `done`, and
      the roadmap epic entry is stamped.
- [ ] CHANGELOG entry for the epic's user-visible effect (cache efficiency / subscription spend),
      written once for the whole epic rather than per story.
- [ ] Standard gate green (build, test, clippy `-D warnings`, fmt, `flux-codegate`).

## Progress
- DONE 2026-07-28. `cache_layout_contract` pins the realized layout for BOTH the `claude`
  (identity-prefix) and `anthropic` segment layouts: union ≤ 4, stable prefix on 1h, rolling tail on
  5m, per-turn segment after the last breakpoint. Its failure message names the budget and points at
  the design doc.
- The invariant is documented in the `flux_providers::messages` module header, where a provider
  author will hit it.
- Live A/B recorded in the design doc, including the negative short-turn results and the warm-cache
  artifact that a naive A/B produces.
- Design doc status moved to implemented; roadmap entry stamped; CHANGELOG entry written once for
  the epic.

## Notes
- Runs last by construction — it validates the other six. If a wave-1 or wave-2 story is dropped or
  closed as no-change-needed, this story's A/B table records that outcome rather than omitting the
  row.
- If the C-133 baseline showed no claude-vs-anthropic delta, say so explicitly in the closing
  section of the design doc. A recorded null result is worth as much as a fix here — it stops the
  question being re-asked.
- Keep the WHATS-NEW / website mirror in sync if the CHANGELOG entry surfaces there; a WHATS-NEW
  edit needs the `website_in_sync` regeneration in the same commit or the workspace test fails.
