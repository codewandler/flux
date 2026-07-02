---
id: L-19
title: flux-lang spec/docs truth pass + render/skill drift guards
pillar: Language
status: done
epic: flux-lang-v1-hardening
design: docs/designs/flux-lang-v1-hardening.md
note: syntax.md documents """ strings, named-arg comma calls, multi-line call args and watch/block that the parser cannot parse; reference.md promises race/throttle/debounce semantics the runtime doesn't deliver; emission-ab.md says "not built" for a shipped arm; render hides obj/list template args from plan approval
---

# flux-lang spec/docs truth pass + render/skill drift guards

## Goal
Every claim in the flux-lang docs is true. Aspirational constructs are marked or removed; docs
describing WS-C-changed semantics are written against the epic design doc's normative "target
semantics"; the two remaining silent-drift holes (render `children()` wildcard, unguarded skill
examples) get the same enforcement the rest of the SSOT surface already has.

## Acceptance
- [x] syntax.md: `"""` multi-line strings, named-arg comma form, multi-line call-arg literals,
      `watch`/`block` spellings marked aspirational or removed; named-args section aligned with
      the L-09 single-object form; edge-case table corrected (F23).
- [x] reference.md: `race` (concurrent first-success), `fallback`, `throttle` (dispatch-counting),
      `debounce` (keyed coalescing) match the L-17 implementation (F24).
- [x] emission-ab.md status updated (arm 1 — strict `emit_plan` schema — shipped); opspec.rs doc
      rot fixed (dangling "unordered.", contradictory order claims, stale positional-binding
      story); error.rs claims match what the analyzer now enforces; STATUS.md RTM rows honest
      (F25). *(all done except error.rs — orchestrator-owned)*
- [x] render.rs: `children()` exhaustive (no `_ =>`); `obj`/`list` template contents visible in
      the plan-approval tree (F26).
- [x] skill.rs: drift test parses every hand-written JSON example as `DraftAst` (mirror of the
      compile.rs:1010 grammar-example guard) (F27).
- [x] Round-trip claim rewording: totality via native-subset + `@json`, citing the L-18 property
      test. Full gate green; CHANGELOG entry. *(rewording done; full gate + CHANGELOG pending the
      concurrent L-15..L-18 landings)*

## Progress
- 2026-07-02 — F23–F27 landed (docs agent; parse.rs/format.rs/runtime.rs/analyze.rs/error.rs left
  to their owning stories):
  - **F23 syntax.md truth pass.** Status section rewritten: `parse_program` module layer documented
    (multi-flow files are real, so dropped from "aspirational"), `@json` list completed
    (`scope`/`saga`/`once`/`checkpoint` added), aspirational list expanded and every affected body
    section marked. `"""` strings, comma-form named args (`grep("ERROR", glob: …)`), comma-kwarg
    flow-control headers, multi-line call-arg literals, `watch`/`block`, `@kind(…)` things, the
    `memo` keyword, `verify … in …`, call-style `expr(…)`/`peek(…)`/`jq(…)` (they parse as op
    calls, not the pure nodes), and file-scope `type` declarations are all explicitly
    aspirational / `@json`-only. Named-arguments section rewritten around the L-09 single-object
    form; `retry`/`confirm`/`throttle`/`debounce`/`race`/`try`/`pipe`/`await` sections corrected;
    `block`→`seq` fixed; edge-case table corrected (empty `parallel` and empty flow body **parse**;
    `until` runs **after** each iteration — verified against the interpreter); complete examples
    rewritten in parseable spellings (real `{role, task}`/`{mined, reviewed}`/… param maps,
    `branch $name` arms, single-line literals); wire-format comparison table fixed (named args are
    a single object in BOTH formats, not "positional array only").
  - **F24 reference.md.** `race`: concurrent, first *success* wins; all-failed = joined branch
    error distinct from timeout; losers' dispatches stay counted/traced. `throttle`: counts **op
    dispatches** per sliding window, atomic bucket keyed by `name` (example gained the required
    `name`). `debounce`: keyed cross-turn coalescing via per-`name` last-trigger in the session
    store. `fallback`: empty-result fall-through made explicit (side-effecting empty branch still
    falls through). `parallel`: declaration-order merge, deterministic prefix on branch failure,
    cross-branch same-symbol binds = analyzer error. Key-invariants bullets updated to match.
  - **F25.** emission-ab.md: arm 1 (strict derived `DraftAst` schema on `emit_plan` via
    `tool_input_schema::<EmitPlanInput>()`) marked **shipped**; the measured A/B remains unbuilt.
    opspec.rs (doc comments only): dangling "unordered." fixed; `input_schema` order-claims made
    consistent (membership load-bearing, order display-only); `required_params` no longer claims
    positional binding (analyzer rejects 2+ positional; canonical call = single named-map object);
    stale positional test comments rewritten. STATUS.md: node count 36/42 → **43**, reliability-tier
    row cites the L-17 target semantics, round-trip row scoped (native subset + `@json` + L-18 name
    guards; property-tested), emission-A/B backlog row un-staled. evolution-impl-plan.md round-trip
    claim scoped the same way.
  - **F26 render.rs.** `children()` is exhaustive — wildcard removed, every node kind an explicit
    arm (future kinds fail compilation). `obj`/`list` template contents render inline in full
    (`{intent: jq(".intent", $extract), n: $count, ok: true}`), including `jq`/`fmt`/`expr` leaves
    (previously `…`); `parse` head now shows its value + target type. New test:
    `obj_and_list_template_contents_are_visible`. Existing render tests green.
  - **F27 skill.rs.** New drift test `body_examples_parse_as_draft_asts` parses all 8 hand-written
    BODY examples as `DraftAst` (mirrors compile.rs
    `grammar_examples_parse_and_use_parallel_for_independent_reads`). All examples were already
    valid — no fixes needed.
  - **memo verification (1f):** no `kw(t, "memo")` in the statement dispatcher and zero `memo`
    occurrences in parse.rs/format.rs — `memo $x = …` does **not** parse; documented as
    `@json`-only.
  - Gate: `cargo test -p flux-lang -q` fully green (179 lib tests + integration suites, incl. the
    two new tests), `cargo clippy -p flux-lang --all-targets -- -D warnings` clean,
    `cargo fmt -p flux-lang --check` clean. All 12 `@json` examples and the 3 rewritten native-text
    examples in syntax.md verified end-to-end through `fluxlang compile`. error.rs wording +
    CHANGELOG + board left to the orchestrator.

## Notes
- Findings F23–F27. Distinct from C-16 (repo-level README/vision/roadmap truth pass) — this story
  is the flux-lang crate docs + language spec surfaces.
