---
id: A-20
title: "BUG: loop re-reads the same files under new symbol names and never converges — stall guard defeated by superficial variation"
pillar: Agent
status: ready
priority: 1
epic: multipass-agent-loop
design: docs/designs/multipass-agent-loop.md
note: "BUG (s_346): 22 read-only rounds, 6 files read ~10× each under renamed symbols, 51.8k output tokens, no answer — stall guard hashes byte-exact transcripts so renamed/reshuffled reads never trip it; make the guard resource-aware"
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
- [ ] **Resource-aware stall/convergence guard.** Track the set of resources already read this turn
      as normalized `op + canonical-args` (path/glob/pattern), **ignoring symbol name and statement
      order**. Failing-first test in `crates/flux-flow`: two consecutive `run_plan` rounds whose
      reads are a subset of already-seen resources (different symbol names) must increment the stall
      counter and, past threshold, escalate then force a `chat` — today they don't (byte-hash differs).
- [ ] **Redundant-read short-circuit (visible).** A `read(path)` for a path already bound in the
      session view returns the cached value with a note (`already read as $X — reusing`) rather than
      re-fetching, so a re-read is costless but legible (extend the A-05 `last_plan_hash` precedent).
      Test: a plan re-reading an already-bound path performs no second IO and the feedback says so.
- [ ] **No-new-evidence convergence counter.** After N (2–3) consecutive rounds that bind **no new
      resource**, feedback escalates ("you've gathered X; no new evidence in N rounds — answer now")
      and the next stalled round forces the honest `chat` termination.
- [ ] **Replay of `s_346` converges.** The captured turn-1 conversation, replayed against the fix,
      terminates with a prose answer (or an honest stop) in ≤ ~6 rounds instead of 18+, without a
      human cancel. Add as a regression fixture.
- [ ] (Consider) a sane default per-turn token budget so a runaway self-terminates.
- [ ] Gate green — `cargo test` · `clippy -D warnings` · `fmt` · codegate layering lint.

## Progress
- 2026-07-03 — Filed from an `s_346` forensic pass (events.db). Root cause + fix shape identified;
  no code yet.

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
