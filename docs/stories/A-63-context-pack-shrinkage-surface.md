---
id: A-63
title: Surface context-pack shrinkage in plain CLI run output
pillar: Agent
status: done
epic: beta-hardening
design: docs/designs/beta-hardening.md
note: "F-011: context-pack member dropping is present in returned metadata but not clearly surfaced in plain CLI run output — a user running normally can't tell members were evicted"
---

# Surface context-pack shrinkage in plain CLI run output

## Goal
When a context pack drops members (eviction/shrinkage), it is visible in the returned metadata but
not clearly surfaced in the plain CLI run output. A user running normally should be able to tell
that context was dropped — silent shrinkage can change answers without any signal.

## Why (evidence)
- Beta F-011: "Context-pack member dropping was visible in returned metadata but not clearly
  surfaced in plain CLI run output."

## Acceptance
- [ ] When a context pack drops members during a run, the plain CLI output shows a concise,
      non-spammy notice (e.g. "context: dropped N of M members") — surfaced once, not per token.
- [ ] The signal is derived from the same metadata already produced (no new bookkeeping), and stays
      quiet when nothing is dropped.
- [ ] Failing-first test: a run that evicts context-pack members emits the shrinkage notice on the
      CLI surface (currently absent).

## Progress
- 2026-07-08 **DONE.** The drop count already existed (`RunEvent::CtxShrunk.dropped`) but never
  reached the sink. `build_ctx` (flux-lang) now emits a `context.shrunk` observation
  (`{ctx, dropped, total}`) alongside the existing shrink record when members are evicted — once per
  shrink, silent when nothing drops (mirrors the compaction path). `CliSink` renders
  `⊙ context: dropped N of M members`. Threaded `sink` through `build_ctx`/`append_ctx`. Test:
  `ctx_shrink_emits_an_observation_for_the_cli` (flux-lang).

## Notes
- The data already exists in returned metadata (per the report) — this is a rendering/surfacing
  change in the CLI sink, not a change to eviction logic.
- Epic: [beta-hardening](../designs/beta-hardening.md).
