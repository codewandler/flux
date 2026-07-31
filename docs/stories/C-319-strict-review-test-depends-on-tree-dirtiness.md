---
id: C-319
title: "`strict_review.rs` reds when the working tree is dirty, and it looks exactly like a real regression"
pillar: Core
status: ready
priority: 7
areas: [flux-sdk]
note: "found by C-304's implementor, which lost real time chasing it — examples/strict_review.flux interpolates the live `git status` and `git diff` into a sub-agent prompt, so past some diff size detect_intent's result truncates into invalid JSON and the loop dies on a field access error"
---

# `strict_review.rs` reds when the working tree is dirty

## Goal

`crates/flux-sdk/tests/strict_review.rs` drives `examples/strict_review.flux`, which interpolates the
**live** `git status` and `git diff` of the checkout it runs in into the reviewer sub-agent's prompt.

Past some diff size, the sub-agent's `detect_intent` result is truncated into invalid JSON and the
loop dies with `field access .kind … of a string`. The test passes against a clean tree — C-304's
implementor verified that three times — and passed the committed gate. But an implementor working a
large story runs the gate with a large uncommitted diff, which is exactly when it fires.

The cost is not the failure; it is the **diagnosis**. The error names a field access in flux-lang and
looks precisely like a real regression in the story being implemented. C-304's implementor chased it
before establishing it was environmental. Every implementor working a large diff will pay that cost
again, and this repo runs many of them in parallel.

This is a close cousin of two scars this project already carries: a test whose verdict depends on
the machine rather than the fixture (the reason `flux test`'s offline client deliberately ignores
`[limits]`, C-307), and a guard tested against its own assumptions. Here the input is the developer's
own working tree.

## Acceptance

- [ ] **Failing-first, and this one is unusual**: a test that *reproduces the environmental failure
      deterministically* — a fixture with a synthetic diff large enough to trigger the truncation —
      before the fix. Reproducing it on demand is most of the work; without that, any fix is a guess
      about a threshold nobody has measured.
- [ ] The test's verdict no longer depends on the dirtiness of the checkout it runs in. Decide how:
      pin the diff to a fixture, cap what is interpolated, or make the example read a supplied diff
      rather than the live one. State the choice and why.
- [ ] **If the truncation itself is the real defect, say so and file it.** A `detect_intent` result
      that truncates into invalid JSON and kills the loop is a provider/parsing failure mode that has
      nothing to do with `git`; the dirty tree is only how it was reached. A fix that merely stops the
      test seeing a big diff would leave that live for any real oversized turn. Decide which layer
      owns it.
- [ ] Grep for other tests that read live repository or machine state — `git`, `$HOME`, the network,
      the clock — and list them. If a second one exists, this story's fix should generalise or the
      story should say why it cannot.
- [ ] Full gate green in both workspaces, and specifically green with a deliberately dirty tree.

## Notes

- Found by C-304's implementor (2026-07-31) while implementing an unrelated story; recorded because a
  test that ambushes implementors costs far more than its own runtime.
- Related: [C-307](C-307-app-run-ignores-limits.md) established the principle this violates — a
  regression gate's verdict must depend only on its fixture, which is why `flux test`'s offline
  client deliberately does not read the local `[limits]` table.
