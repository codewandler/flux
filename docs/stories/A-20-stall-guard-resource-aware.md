---
id: A-20
title: "BUG: loop re-reads the same files under new symbol names and never converges — stall guard defeated by superficial variation"
pillar: Agent
status: done
epic: multipass-agent-loop
design: docs/designs/multipass-agent-loop.md
note: "FIXED — dispatch-time ReadTracker keys reads on op + resolved args (symbol renames invisible by construction): no-new-evidence rounds escalate at 2 / force the honest stop at 3, exact-repeat fs reads are cache-served with an 'already read as $X — reusing' note (write-invalidated, cap-scope-aware), and the s_346 shape is pinned as a fixture (≤6 rounds, 3 real reads, was 22 rounds/51.8k tokens)"
---

# BUG: loop re-reads the same files under new symbol names and never converges

## Goal
Make the agent loop **converge** on read-heavy analysis turns instead of spinning read-only rounds
until the iteration cap or a human cancel. Serves the Agent pillar's "the loop must terminate
honestly" contract: an agent that has already gathered the evidence must answer, not re-fetch it.

## Symptom (session `s_346`, 2026-07-02, ai-agents repo, Sonnet 4.6)
Task: *"analyze why creating a knowledge source → HTTP 404, do not implement."* The loop ran
**1 orient → 3 gather → 18 execute** rounds, **all pure reads**, produced **no answer**, and was
cancelled by the user. Hard evidence from the event log:

- Same 6 files re-read every round under fresh symbol names — `knowledge-api.ts` **11×**, `main.rs`
  **11×**, `lib.rs` **11×**, `author-api/lib.rs` **10×**, `storage.rs` **10×**, `vite.config.ts` **6×**
  (`$main_rs` → `$main_rs_full` → `$main_rs_content`; `$author_lib` → `$author_api_lib` →
  `$author_router_full` → `$author_router_body` …). ~53 reads where 6 would do (~10× waste).
- **Every read succeeded** (`failure: null` on all 221 observations) — nothing was missing; it
  re-fetched content it already held.
- The evidence needed to answer was complete by execute round ~8 (backend `POST /knowledge` route +
  frontend client + `vite.config.ts` dev proxy all read) — the loop kept reading anyway.
- Turn 1 burned **51,872 output tokens** for a cancelled turn.

## Root cause
Three convergence backstops in `crates/flux-flow/src/loop_host.rs` all failed:

1. **Primary — stall guard keys on a byte-identical transcript.** `guard_transcript_with_key`
   (`loop_host.rs:550`) SHA-256s the raw `run_plan` transcript and only counts a stall when two
   consecutive transcripts are byte-identical. Re-reading the same files under different symbol
   names / in a different order / in a slightly different subset changes the transcript every round,
   so `transcript_stall` never reaches `STALL_ESCALATE` (2) or `STALL_STOP` (4). Confirmed:
   **`loop-guard` fired 0 times** the entire session.
2. **No token ceiling by default.** A-10 `token_budget` is `None` unless set via flag/env/config,
   so the 51.8k-token runaway had no automatic stop.
3. **Execute phase applies no "you've gathered enough — answer now" pressure.** Gather is capped at
   3 rounds then silently spills into execute (`assets/agent-loop.flux:24`); the label changes, the
   behavior doesn't. The model even re-emitted **5 reworded briefs** of the same goal, so the
   host-carried brief's "still need:" never converged to empty. The harness already renders
   `"Existing session symbols (reference these instead of re-fetching)"` (`compile.rs:1126`) — the
   model ignores it, and nothing makes ignoring it costly.

Same *class* as [A-05](A-05-legible-silent-success-feedback.md) (loop re-running succeeded ops) and
the empty-parallel-branch re-gather loop, but a distinct mechanism: renamed-symbol re-reads slipping
past a byte-exact stall hash.

## Acceptance
- [x] **Resource-aware stall/convergence guard.** Track the set of resources already read this turn
      as normalized `op + canonical-args` (path/glob/pattern), **ignoring symbol name and statement
      order**. Failing-first test in `crates/flux-flow`: two consecutive `run_plan` rounds whose
      reads are a subset of already-seen resources (different symbol names) must increment the stall
      counter and, past threshold, escalate then force a `chat` — today they don't (byte-hash differs).
      → `renamed_symbol_rereads_escalate_then_force_stop` (fails with the tracker disconnected).
      Keyed at the DISPATCH seam on `op + resolved args` — post-var-resolution, so symbol-name and
      order insensitivity hold by construction, dynamic paths included.
- [x] **Redundant-read short-circuit (visible).** A `read(path)` for a path already bound in the
      session view returns the cached value with a note (`already read as $X — reusing`) rather than
      re-fetching, so a re-read is costless but legible (extend the A-05 `last_plan_hash` precedent).
      Test: a plan re-reading an already-bound path performs no second IO and the feedback says so.
      → `redundant_read_short_circuits_io_with_reuse_note` + the correctness twin
      `mutating_op_invalidates_read_cache` (any local-state-mutating dispatch clears the cache; no
      stale reuse after a write). Scope: turn-local (reset in `set_turn`), fs-scoped read ops only
      (effects ⊆ {Read, Filesystem} ∧ access == [Filesystem]); `evidence`/`metrics` (live session
      state) are stall-tracked but never cache-served; no hit while a `with_tools` scope is open.
- [x] **No-new-evidence convergence counter.** After N (2–3) consecutive rounds that bind **no new
      resource**, feedback escalates ("you've gathered X; no new evidence in N rounds — answer now")
      and the next stalled round forces the honest `chat` termination.
      → `RESOURCE_STALL_ESCALATE = 2`, `RESOURCE_STALL_STOP = 3`; reset on any fresh read /
      effectful round / read-free round (`fresh_read_resets_resource_stall`).
- [x] **Replay of `s_346` converges.** The captured turn-1 conversation, replayed against the fix,
      terminates with a prose answer (or an honest stop) in ≤ ~6 rounds instead of 18+, without a
      human cancel. Add as a regression fixture.
      → `s346_renamed_reread_turn_is_forced_to_converge`: the full built-in `agent-loop.flux`
      driven by a scripted planner that re-emits the captured SHAPE (same 3 files, fresh symbol
      names every round, forever) — converges in ≤ 6 planner rounds, ends with the honest stop, and
      performs exactly 3 real read dispatches. A synthetic-scripted fixture rather than a literal
      transcript replay: hermetic and maintainable, and the fix alters the round sequence after
      round ~4 anyway, so a byte-replay of the original 22 rounds cannot exist post-fix.
- [ ] ~~(Consider) a sane default per-turn token budget so a runaway self-terminates.~~ **Deferred
      deliberately** — A-26 (review-hardening) fixed the budget meter (cumulative billed tokens)
      the same day; choosing a default ceiling is a separate product decision on top of the now-
      correct meter, and the resource guard already self-terminates this runaway class.
- [x] Gate green — `cargo test` · `clippy -D warnings` · `fmt` · codegate layering lint.

## Progress
- 2026-07-03 — Filed from an `s_346` forensic pass (events.db). Root cause + fix shape identified;
  no code yet.
- 2026-07-03 — Picked up (in-progress). Design: dispatch-time read tracking in `ExecutorHost`
  (loop-host-only seam) — a turn-scoped `ReadTracker` records every pure-read dispatch under a
  canonical `op:args` key (post-var-resolution, so symbol renames can't vary it), serves cache hits
  with an `already read as $X — reusing` note, is invalidated by any non-read dispatch, and feeds a
  `resource_stall` counter in `LoopGuard` (escalate at 2 no-new-evidence rounds, force-stop at 3).
  Overlap check: A-26/A-27 (review-hardening, concurrent session) touch the same guard — kept
  disjoint; acceptance-5 default budget deferred to A-26 (the budget meter undercounts today).
- 2026-07-03 — **DONE.** Landed: `OpHost::call_bound` (flux-lang, default no-op post-bind
  notification) + `ReadTracker`/`classify_spec`/`resource_key` (flux-flow `runtime.rs`, with the
  dispatch interception in `ExecutorHost::dispatch` — built on top of L-32's `dispatch_outcome`
  version) + `LoopGuard.resource_stall`/`guard_resources` (loop_host, applied after
  `guard_transcript` on the clean-success path only; halt/suspension paths untouched — the
  failure-stall owns those). 5 new tests, each proven failing with the tracker disconnected
  (`None` at the wrapper call) and green with it. Full gate green on a tree shared with the
  concurrent review-hardening session's 12 landed stories. Residual idea (not filed): at the
  resource force-stop, render a REAL final answer via the A-06 completion fast-path instead of
  the canned stop — the evidence is complete by definition at that point.

## Notes
- Diagnosis method: `sqlite3 ~/.flux/events.db` over `stream='s_346'` — `plan_attempted` (per-round
  plan_text + phase), `observation` (`turn.iteration`/`turn.gather`/`flow.brief`, all `failure:null`),
  `turn_ended` (`outcome=cancelled`, usage). Reusable for future loop postmortems.
- Key code: `crates/flux-flow/src/loop_host.rs` — `guard_transcript_with_key` (:550), `LoopGuard`
  (:329), `STALL_ESCALATE`/`STALL_STOP` (:71), `token_budget` (:395); `run_plan` identical-plan skip
  (`last_plan_hash`). Loop skeleton: `crates/flux-flow/assets/agent-loop.flux`. Symbol surfacing:
  `crates/flux-flow/src/compile.rs:1126` `symbols_block`.
- The model-side discipline gap (renaming symbols, rewording the brief) is real but should be made
  *costless to ignore* by the harness rather than relied upon.
