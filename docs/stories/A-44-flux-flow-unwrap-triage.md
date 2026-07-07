---
id: A-44
title: flux-flow unwrap/expect triage — classify the non-test sites, convert the fallible ones
pillar: Agent
status: done
design:
epic:
note: ~100 non-test unwrap/expect in flux-flow (heuristic count; workspace outlier — next is flux-capabilities at ~39); AGENTS.md bans unwrap on fallible IO in non-test code
---

# flux-flow unwrap/expect triage — classify the non-test sites, convert the fallible ones

## Goal
flux-flow — the crate that runs the one agent loop — is the workspace's clear outlier for
production `unwrap()/expect()` (~100 non-test sites vs ~39 for the runner-up). Audit-first, not
blanket conversion: the count includes infallible unwraps (poisoned-lock policy, static regexes,
known-good serde) where conversion is churn. Classify every site, convert the genuinely fallible /
model-input-reachable ones, and record the classification so the next reviewer doesn't re-litigate.

## Acceptance
- [x] A classification table (in this story or a short design note): every non-test
      unwrap/expect in flux-flow bucketed as infallible-by-construction / fallible-convert /
      fallible-accept-with-reason.
- [x] Every `fallible-convert` site converted to `flux_core::Result`, a diagnostic, or graceful
      degradation — each behavioral change with a failing-first test (a crafted input that panics
      before, errors cleanly after). (Vacuous: the audit found zero bucket-(b) sites — see Progress.)
- [x] A recorded go/no-go decision on a crate-local clippy `disallowed-methods` gate for
      unwrap/expect in the loop-critical modules (precedent: flux-providers' bare-serde ban) —
      adopted only if signal/noise is acceptable; the decision + reasoning land in Notes.
- [x] Full gate green (the loop is on the production path — no panics introduced, none merely
      relocated into `expect` with a longer message).

## Triage

Enumerated every non-test `.unwrap(`/`.expect(` in `crates/flux-flow/src` (excluding `#[cfg(test)]`
modules/files and inline `#[cfg(test)]` items) by finding each file's `mod tests` boundary and any
earlier standalone `#[cfg(test)]` items, then reading every remaining site in context. Result:
**101 non-test sites**, matching the ~100 heuristic. `agent_sink.rs`, `lib.rs`, `engine.rs`,
`registry.rs`, and all of `voice/` have zero non-test sites.

| Site(s) (file:line) | Bucket | Reason |
|---|---|---|
| `composites.rs`:94,102,122,129,134,147,171 | (a) infallible-by-construction | `self.state.lock().unwrap()` — poisoned-lock policy (a `std::sync::Mutex` only poisons if another thread already panicked while holding it; escalating here is the intended, crate-wide convention, not a silently-swallowed failure) |
| `composites.rs`:236 | (a) infallible-by-construction | `program.ops.into_iter().next().expect("checked len")` — `has_only_one_op` (checked 12 lines above) requires `program.ops.len() == 1` |
| `compile.rs`:1769 | (a) infallible-by-construction | `messages.last_mut().expect("a prior user message exists by construction...")` in `push_truncation_repair` — verified inductively: `EngineLoopHost::plan` (loop_host.rs) only ever hands `compile_turn` a `conversation` that is the persisted history (which always still ends in *this turn's* persisted user instruction — intermediate rounds are never persisted) plus optionally an appended ephemeral User message (resume-context or feedback), and the degenerate-empty case pushes one too — so the vector's tail is User at every round's start, and the loop's own alternation invariant (assistant/tool-result push pairing, same precedent as `hidden_ops_rejection`) preserves that through every step. Covered by `compile_turn_repairs_max_tokens_truncation_with_empty_preamble`. |
| `compile.rs`:2006-2008 | (a) infallible-by-construction | `extract_json(chunk).expect(...)` / `serde_json::from_str(&json).expect(...)` over `ast_grammar()`'s compile-time-constant string, guarded by the `text_grammar_examples_parse_and_match_the_json_arm` test (doc comment on `text_grammar`/`build_text_grammar` already states this) |
| `loop_host.rs`: 545,571,573,581,583,584,586,591,592,595,604,611,623,632,633,639,647,674,753,807,821,867,883,931,943,947,956,968,991,1000,1018,1040,1042,1043,1054,1056,1063,1065,1068,1091,1092,1099,1125,1129,1177,1186,1239,1318,1336,1443,1477,1519,1540,1606,1615,1654,1769,1772,1775,1778,1781,1784,1787,1790 (64 sites) | (a) infallible-by-construction | `.lock().unwrap()` on one of `EngineLoopHost`'s internal `Mutex` fields (`turn`/`guard`/`usage`/`calls`/`pending_completion`/`brief`/`last_phase`/`resume_context_shown`/`provider`/`model`/`token_budget`/`readonly_ladder`/`conversation_cache`) or the `SharedSink` wrapper's inner lock — poisoned-lock policy, same reasoning as `composites.rs` above; spot-checked several critical sections and none contain a reachable panic that would poison the lock for a later caller |
| `loop_host.rs`:550 | (a) infallible-by-construction | `slot.lock().unwrap().take().expect("host captured")` in `EngineLoopHost::install` — the `Arc::new_cyclic` closure synchronously fills `slot` (`*slot2.lock().unwrap() = Some(host.clone())`) before this line runs; no `.await` or early return sits between the two |
| `loop_host.rs`:1676,1693 | (a) infallible-by-construction | `scope.path_for(&decl.name).expect("project/global has path")` — the enclosing `match scope { CompositeScope::Project => ..., CompositeScope::Global => ... }` arm structurally binds `scope` to exactly the variant `path_for` returns `Some(_)` for (`composites.rs`'s `path_for` only returns `None` for `Turn`/`Session`, and neither arm here is reached for those) |
| `runtime.rs`: 200,206,214,224,239,247,253,276,310,344,429,514,521 (13 sites) | (a) infallible-by-construction | `.lock().unwrap()` / `.cap_scope_guards.lock().unwrap()` on `Executor`'s internal `Mutex` fields — poisoned-lock policy |
| `state.rs`: 243,255,280,306,415,430,464,482,517,530 (10 sites) | (a) infallible-by-construction | `self.conn.lock().unwrap()` guarding the sqlite `Connection` — poisoned-lock policy; every fallible SQL operation inside the guarded block already returns through `.map_err(map_sql)?` (spot-checked `put_value`/`get_value`), so the lock unwrap is the only panic surface and it is the deliberate policy, not a substitute for error handling |

**Bucket totals: (a) infallible-by-construction = 101. (b) fallible-convert = 0. (c)
fallible-accept-with-reason = 0.**

No conversions were needed (bucket (b) is empty) — see Progress for how hard this was checked
before accepting a zero count.

## Progress
- Enumerated all 14 source files under `crates/flux-flow/src`, located each file's `#[cfg(test)]`
  boundary (both the trailing `mod tests` block and, in `compile.rs`/`loop_host.rs`, standalone
  inline `#[cfg(test)]` items that sit *before* the file's `mod tests`), and counted only sites
  strictly before those boundaries. 101 non-test sites total (compile.rs 3, composites.rs 8,
  loop_host.rs 67, runtime.rs 13, state.rs 10; agent_sink.rs/lib.rs/engine.rs/registry.rs/voice/*
  all 0).
- Classified every site by reading it in context (not by pattern-matching alone): 94 are
  `.lock().unwrap()` on an internal `std::sync::Mutex` (poisoned-lock policy, uniform across the
  crate — confirmed no `RwLock` usage and spot-checked that fallible work inside the guarded
  sections already returns `Result` rather than unwrapping further); the remaining 7 are `.expect()`
  calls each backed by either a check on the immediately preceding lines, a `match`-arm structural
  guarantee, or (the one site worth real scrutiny) a crate-architecture invariant — traced through
  `EngineLoopHost::plan`'s persistence design to confirm `compile_turn`'s `conversation` argument
  always ends in a User message at every round of a turn, not just the first.
- Result: **zero bucket-(b) fallible-convert sites**. Nothing was reachable from model-emitted
  plans, provider bytes, or store contents that wasn't already funneled through `flux_core::Result`
  before the `.unwrap()`/`.expect()` boundary — the crate's ~100-site outlier count is explained
  entirely by its unusually mutex-heavy `EngineLoopHost`/`Executor`/`FlowStore` state design, not by
  unhandled fallibility. No code changes were made as a result (Acceptance item 2 is vacuously
  satisfied); no new tests were added because there was no panic-before/error-after behavior change
  to cover.
- Clippy `disallowed-methods` gate: **no-go** (see Notes for the full reasoning).
- Gate run as a baseline check (no code touched): `cargo test -p flux-flow` — 247 + 3 + 1 passed, 0
  failed; `cargo clippy -p flux-flow --all-targets -- -D warnings` — clean; `cargo fmt -p flux-flow
  -- --check` — clean. All green before this story and unchanged after it.
- Status: **done**. Full audit completed in one session; no partial/deferred scope.

## Notes
- Ground severity against the envelope invariants before filing anything found en route as its own
  bug (review-grounding rule): a panic in the loop host is availability, not authorization. Nothing
  found in this triage touches permission/authorization/redaction logic — every site is internal
  loop-host/executor/store state management. No candidate story to file from this pass.
- The ~100 figure is a heuristic (counts before the first `#[cfg(test)]` per file) — expect the
  real fallible set to be meaningfully smaller; that's the point of triage-first. Confirmed: 101
  counted, 0 turned out fallible-convert.
- **Clippy `disallowed-methods` gate decision: NO-GO.** Reasoning: bucket (a) alone is all 101
  sites (there is no bucket (c) to add). A crate-wide `unwrap`/`expect` ban in `clippy.toml` (the
  `flux-providers` bare-serde precedent) would require either (1) ~101 individual
  `#[allow(clippy::disallowed_methods)]` annotations — pure noise, obscuring any future genuinely
  fallible unwrap a reviewer should catch — or (2) a blanket module/file-level `#![allow(...)]`,
  which defeats the gate's purpose for all *future* code in the same modules too. The 94 mutex-lock
  sites *could* in principle be centralized behind a single `fn lock(&self) -> MutexGuard<'_, T>`
  helper per guarded struct (collapsing 94 allows into a handful), which would make a future gate
  low-noise — but that's a struct-shape refactor across `loop_host.rs`/`runtime.rs`/`state.rs`, out
  of scope for a triage story and not something this pass should do as a side effect. Flagging as a
  possible follow-up if the crate ever wants this lint, not filing it as a story now (no bug behind
  it, just a possible future hygiene improvement).
