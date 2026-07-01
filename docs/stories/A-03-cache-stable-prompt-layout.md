---
id: A-03
title: Cache-stable prompt layout — stop re-writing the ~34k prefix on every call
pillar: Agent
status: done
note: FIXED — name-sorted registry/catalog + segmented cache-first system prompt (`SystemSegment` on `Request`; planner catalog/grammar and identity blocks carry breakpoints, per-turn symbols ride uncached after them); live-verified 99% cross-process cache hit ($0.1199 → $0.0106, 11.3×) and 96% in-session hit on a 12-step turn
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
- [x] Failing-first: `ToolRegistry::specs()`/`names()`/`active_specs()` and
      `OpRegistry::signatures()`/`op_names()` return name-sorted results
      (`flux-runtime::tests::registry_specs_and_names_are_name_sorted`; registration order is
      deliberately non-alphabetical).
- [x] Failing-first: the planner system is assembled twice with a *changed symbol set* — segments A
      (planner instructions + catalog + grammar) and B (base system) are byte-identical, symbols
      ride in a trailing UNCACHED segment
      (`compile::tests::system_segments_keep_the_static_prefix_stable_across_symbol_changes`).
- [x] The Messages body builder renders segmented system prompts as a block array with
      `cache_control` on the marked segments only, and joins plain when caching is off
      (`segmented_system_renders_breakpoints_on_cached_segments_only`,
      `segmented_system_joins_plain_when_caching_is_off`). Implemented as
      `flux_provider::SystemSegment` + `Request.system_segments` (+ `Request::system_text()` for
      codecs without a breakpoint notion — OpenAI Chat/Responses join in order, preserving the
      stable-first prefix for their implicit caching; the subscription `system_prefix` prepends as
      its own cached segment).
- [x] Live (Bedrock sonnet, same workspace): first `flux run "say ok"` writes 31.7k ($0.1199); the
      second run seconds later reads **99%** from cache ($0.0106 — **11.3× cheaper**). In-session:
      a 12-step multi-plan turn ends at **96% hit**, $0.0681 total.
- [x] `flux usage` rows now label `cache read` and `cache write` distinctly (the CLI turn
      annotation already did).

## Progress
- **DONE (2026-07-02).** Registry sort in `flux-runtime` (`specs`/`names`/`active_specs`) +
  `OpRegistry` (`signatures`/`op_names`). New `SystemSegment` seam: `Request.system_segments`
  (flux-provider), rendered by `build_messages_body::segmented_system_field` (breakpoints per
  marked segment; plain join when caching off); `compile_turn` assembles
  `assemble_system_segments` = [A planner-static (cached), B base-system (cached), C symbols
  (uncached)] — `build_planner_prompt` no longer takes the view, so segment A is byte-stable.
  OpenAI codecs read `system_text()`. Mock/test providers matching on `req.system` updated to
  `system_text()` (strict-review journey/SDK fixtures, orchestrate delegator probe, loop-host
  recorder). `flux usage` labels reads vs writes. Full gate green.

## Notes
- Found during the 2026-07-01 harness e2e review; numbers from Bedrock (`aws`) but the layout defect
  is provider-independent — any Anthropic-caching provider (anthropic/claude/openrouter-anthropic)
  misses identically.
- A-04 (gated catalog) shrinks the same prompt; do A-04 first or together — a smaller static prefix
  is also a cheaper cache write when it does miss.
- Per-turn `conversation` feedback is already an ephemeral **user message** (correct side); only the
  system-side dynamics need to move.
