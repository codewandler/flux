---
id: A-09
title: Planner prompt prefers parallel for independent reads; one-shot compile repairs ride cached segments
pillar: Agent
status: done
priority:
note: the grammar now teaches `parallel` for independent reads (each stays for ordered/dynamic-list iteration; all worked examples parse-tested against the real AST), and one-shot compile() repairs ride a byte-stable cached system segment with the repair exchange as messages
---

# Planner prompt prefers parallel; cache-segmented one-shot compile

## Goal
Two planner-surface efficiency fixes, verified 2026-07-02. (1) The grammar hint says "prefer
`parallel` for independent reads" (`crates/flux-flow/src/compile.rs:709`) but the very next worked
example — "read a.rs, b.rs and c.rs and summarise each" — is written with `each`
(`compile.rs:716-721`), which executes strictly serially (`crates/flux-lang/src/runtime.rs:1303-1343`);
`parallel` branches run concurrently (`try_join_all`, `runtime.rs:1496-1526`). The canonical example
teaches the slow shape. (2) The one-shot `compile()` path (`compile.rs:131-181`) builds
`repair_prompt` = entire base prompt + previous output + error (`compile.rs:840-845`) and sends it
through `run_model` as one concatenated string with NO cache segments (`compile.rs:456-471`) — each
repair roughly doubles uncached input, while the loop path (`compile_turn`) already keeps its bulk
cached.

Deliberately rejected: concurrent `each` — the loop item binds through the shared session store
each iteration (last-writer-wins race under concurrency); `parallel` already has per-branch
buffered sinks + distinct branch names. The prompt is the right lever.

## Acceptance
- [x] **Failing-first:** `grammar_examples_parse_and_use_parallel_for_independent_reads`
      (flux-flow) — extracts both example JSON snippets from `ast_grammar()`, parses them into
      `DraftAst`, analyzes against a registry with `read`, and asserts the independent-read
      example's top node is `parallel` (fails today: it's `each`; also permanently guards the
      hand-written examples against AST schema drift — nothing validates them today).
- [x] The independent-read example is a `parallel` node with named branches; an `each` example
      remains for an explicitly order-dependent case; the `:709` hint reads "prefer `each` for
      **ordered** iteration; use `parallel` whenever reads/calls are independent."
- [x] **Failing-first:** `oneshot_repair_reuses_a_byte_stable_cached_segment` (flux-flow) — with a
      request-recording provider responding `[garbage, valid]`, assert 2 requests with identical
      `system_segments`, `segments[0].cache == true`, the instruction as a user message, and the
      error text only in request 2's messages (fails today: `system_segments` is empty).
- [x] One-shot `compile()` builds a segmented `Request` (segment A cached: instructions + catalog +
      grammar, byte-stable across attempts; symbols uncached; repair context rides as messages);
      `build_oneshot_prompt`/`repair_prompt`/`run_model` deleted if orphaned (clean cutover).
- [x] Full gate green; CHANGELOG entry.

## Progress
- Filed 2026-07-02 from the harness claims review (P3 of the round).
- Done 2026-07-02. Grammar now carries THREE worked examples — sequential (readme→grep, dependency
  chain), `parallel` with named branches for the independent-reads case, and `each` explicitly
  scoped to dynamic lists (`parallel` branches are static) — with the tightened hint stating `each`
  runs strictly in order. The parse-test covers every example permanently. One-shot `compile()`
  rebuilt on the segmented Request: new `build_oneshot_system` (segment A, cached, byte-stable),
  symbols as the uncached trailing segment, instruction + repair exchange as messages;
  `build_oneshot_prompt`/`repair_prompt`/`run_model` deleted (clean cutover; flux-cognition's
  private `run_model` is a separate helper). `SystemSegment` gained `PartialEq/Eq` for the
  byte-stability assertion.

## Notes
- Blast radius of (2), verified: one-shot `compile` is called only by
  `flux_sdk::FlowClient::compile/run` (flux-sdk/src/flow.rs:288-300, :445) + compile.rs tests.
- Design decisions in `~/.claude/plans/wiggly-tumbling-salamander.md` §A-09.
