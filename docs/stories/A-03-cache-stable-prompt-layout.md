---
id: A-03
title: Cache-stable prompt layout — stop re-writing the ~34k prefix on every call
pillar: Agent
status: ready
priority: 2
note: prompt caching NEVER hits — HashMap-ordered op catalog shuffles the prompt per process, and per-turn symbols sit inside the single cache_control block; every provider call pays ~34k cache-WRITE (1.25× input), ~10× cost overhead
---

# Cache-stable prompt layout — stop re-writing the ~34k prefix on every call

## Goal
Make the planner prompt cache-friendly so consecutive calls hit the provider prompt cache instead of
re-writing the full prefix. Measured live (2026-07-01, Bedrock sonnet, tiny scratch repo): **every**
call — including the second turn of the SAME session — reports `cache_read: 0` and
`cache_creation: ~34k` (REST probe: turn 1 `34128`, turn 2 `34105`). Two identical `flux run "say ok"`
invocations seconds apart each cost $0.1284 (full cache write) instead of ~$0.014 (cache read). A
4-trivial-turn server session totalled ctx 138k / $0.40 (`flux usage`, s_309). At the token level the
"token efficiency" pillar claim is currently inverted: cache-write bills at 1.25× input, so flux pays
a ~25% premium over not caching at all.

Root causes (all verified in code):
1. **Non-deterministic catalog order:** `ToolRegistry.tools` is a std `HashMap`; `specs()`/`names()`
   iterate it directly (`crates/flux-runtime/src/lib.rs:349–396`), and `ops_catalog` renders in that
   order (`crates/flux-flow/src/compile.rs:646`) → the system prompt is byte-shuffled on every
   process start → cross-process cache hits are impossible.
2. **Dynamic content inside the cached block:** `build_planner_prompt` embeds `symbols_block(view)`
   (`compile.rs:723,752`) — the per-turn session symbols — *before* the grammar, and the whole
   system prompt is ONE `cache_control` block (`crates/flux-providers/src/messages/mod.rs:114`).
   Any symbol change invalidates the entire prefix → in-session misses (34128 → 34105 drift).
3. Skills + git context (working-tree status) are also prepended into the same single block
   (`engine.rs::base_system_with_skills`, `flux-runtime::context`) — legitimate context, wrong side
   of the breakpoint.

## Acceptance
- [ ] Failing-first: `ToolRegistry::specs()`/`names()` (and `OpRegistry::signatures()`) return
      name-sorted results — a test registers ops and asserts sorted order (fails on HashMap order
      whenever the seed differs).
- [ ] Failing-first: build the planner system prompt twice for the same session with a *changed
      symbol set* — the static prefix (identity + instructions + catalog + grammar) must be
      byte-identical, with per-turn material (symbols, skills-if-changing, context) placed after the
      cache breakpoint or in messages. Assert on the block structure the codec emits (multi-block
      `system` array with `cache_control` on the last *static* block).
- [ ] The Messages body builder supports a static/dynamic system split (array of blocks, breakpoint
      on the static head) — unit test pins the emitted JSON shape.
- [ ] Live: second `flux run --yes "say ok"` in the same workspace within the cache TTL reports
      `cache_read ≈ prefix size` and `cache_creation ≈ 0` (visible in the turn annotation /
      `flux usage`); in-session turn 2 likewise.
- [ ] The turn annotation renders cache reads AND writes distinctly (today a session line shows only
      `cache write`, and `flux usage` labels writes ambiguously as `cache`).

## Progress
- (not started)

## Notes
- Found during the 2026-07-01 harness e2e review; numbers from Bedrock (`aws`) but the layout defect
  is provider-independent — any Anthropic-caching provider (anthropic/claude/openrouter-anthropic)
  misses identically.
- A-04 (gated catalog) shrinks the same prompt; do A-04 first or together — a smaller static prefix
  is also a cheaper cache write when it does miss.
- Per-turn `conversation` feedback is already an ephemeral **user message** (correct side); only the
  system-side dynamics need to move.
