---
id: C-308
title: "A red test hides from the workspace gate: `flux-lang --features cli` fails on main"
pillar: Core
status: ready
priority: 6
areas: [flux-lang, ci]
note: "L-96 made `confirm \"y\", risk: high` VALID canonical syntax, so a test using it as malformed input now parses and its expect_err panics. Nothing in the workspace enables flux-lang's `cli` feature, so `cargo test --workspace` is green while the dev loop documented in crates/flux-lang/AGENTS.md is red"
---

# A red test hides from the workspace gate: `flux-lang --features cli` fails on main

## Goal

Make the failure visible, then fix it — in that order, because the visibility gap is the more
expensive half.

## The defect

`crates/flux-lang/src/bin/fluxlang.rs` → `tests::rail_reports_the_existing_parser_diagnostics` uses
`confirm "y", risk: high` as its *malformed* input. **L-96 made that spelling valid canonical
syntax**, so the source now parses cleanly and the test's `expect_err` panics.

Confirmed on `main` at the time of filing:

```
$ cargo test -p codewandler-flux-lang --features cli --bin fluxlang
test tests::rail_reports_the_existing_parser_diagnostics ... FAILED
test result: FAILED. 10 passed; 1 failed
```

**The fix itself is one line** — pick input that is still malformed. That is not what this story is
about.

## Why this is worth a story rather than a drive-by

`cargo test --workspace` is **green**, because nothing in the workspace enables `flux-lang`'s `cli`
feature. But `crates/flux-lang/AGENTS.md` documents a dev loop that *does* pass `--features cli`. So
the project's own documented command has been red while every gate — local and CI — reported green.

That is the interesting failure: **a feature-gated test target that no gate exercises is a test that
does not exist**, and it decays silently the moment the language moves under it. This one was found
only because an unrelated implementor happened to run the documented command.

## Acceptance

- [ ] `rail_reports_the_existing_parser_diagnostics` passes, using input that is genuinely malformed
      under **current** canonical syntax. Do not weaken the assertion to make it pass — the test
      exists to prove the rail surfaces parser diagnostics, so it must still prove that.
- [ ] **The gate sees this target.** Either the root gate grows a `--features cli` leg, or CI does, or
      the feature is removed as a concept. Whichever is chosen, a failure in this target must red a
      run that someone actually watches. State the choice and why.
- [ ] **Audit for siblings.** Enumerate every feature-gated test target in both workspaces and report
      which are exercised by a gate and which are not. `--no-default-features` and
      `--features postgres` are already run for some crates; this is asking for the complete picture,
      not a spot check. Anything unexercised is in the same class as this bug and should be listed
      even if not fixed here.
- [ ] Full gate green, plus the newly-covered leg.

## Notes

- Found 2026-07-31 by L-97's implementor while working in `flux-lang`, and independently reproduced
  before filing. It is genuinely pre-existing — it reproduces at `c5c69fed` with none of L-97's code.
- ⚠ Related class, worth keeping in mind while auditing: `AGENTS.md` already records that this
  machine has `bwrap` where CI runners do not, so a sandbox test can pass locally and red CI. That is
  the same shape of defect — an environment or feature dimension the default gate does not cover —
  approached from the other direction.
- Related: [L-96](L-96-canonical-named-option-headers.md) made the option-header spelling canonical
  and is what invalidated the fixture; [L-97](L-97-flux-glyph-notation.md) found it.
