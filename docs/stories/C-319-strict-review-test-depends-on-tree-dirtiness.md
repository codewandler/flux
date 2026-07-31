---
id: C-319
title: "`strict_review.rs` reds when the working tree is dirty, and it looks exactly like a real regression"
pillar: Core
status: done
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

- [x] **Failing-first, and this one is unusual**: a test that *reproduces the environmental failure
      deterministically* — a fixture with a synthetic diff large enough to trigger the truncation —
      before the fix. Reproducing it on demand is most of the work; without that, any fix is a guess
      about a threshold nobody has measured.
      → `strict_review_reviewer_prompts_come_from_the_fixture_not_the_live_checkout`
      (`crates/flux-sdk/tests/strict_review.rs`). It reproduces the *dependency* deterministically —
      RED at the merge base, printing the developer's own `git status` as the reviewer prompt's
      status section. It does **not** reproduce the truncation, because the truncation does not
      exist: see the third box.
- [x] The test's verdict no longer depends on the dirtiness of the checkout it runs in. Decide how:
      pin the diff to a fixture, cap what is interpolated, or make the example read a supplied diff
      rather than the live one. State the choice and why.
      → **Pinned to a fixture, at the test's op seam.** `git_status`/`git_diff` are replaced with
      `PinnedRepositoryRead` over `tests/fixtures/strict_review/{status.txt,diff.patch}`, and the
      reviewed file is `tests/fixtures/strict_review/subject.rs` instead of a live crate source.
      *Not* the example: `examples/strict_review.flux` reading the live tree is the whole point of
      `flux review`, it is a shipped artifact, and its text is frozen by SHA-256 in
      `crates/flux-lang/tests/cst_agreement.rs`. *Not* a cap either: capping what is interpolated
      would still leave the verdict a function of the checkout, just a shorter one.
- [x] **If the truncation itself is the real defect, say so and file it.** A `detect_intent` result
      that truncates into invalid JSON and kills the loop is a provider/parsing failure mode that has
      nothing to do with `git`; the dirty tree is only how it was reached. A fix that merely stops the
      test seeing a big diff would leave that live for any real oversized turn. Decide which layer
      owns it.
      → **The truncation is not reproducible and no code path produces it; layer (a) — the test —
      owns this.** Measured: live diffs of 336 KB, 5.2 MB and 17.4 MB all leave both strict-review
      tests green, and a full `cargo test --workspace` green. Audited: the only truncator on the op
      path is `flux_runtime::trim_tool_output` (cap 20 000 chars, `FLUX_TOOL_OUTPUT_CAP`), applied
      **only to the transcript view** — `crates/flux-lang/src/runtime.rs:1862,1883,2173,2265`, each
      followed by `last = outcome.view` and "the canonical value is untouched"; the bound value is
      `result.content` (`runtime.rs:465`), never trimmed by the store
      (`crates/flux-flow/src/state/mod.rs:255`) or by dispatch. `detect_intent` returns
      `ToolResult::ok(out.to_string())` (`crates/flux-tools/src/reflect.rs:223`) — a serialized
      `serde_json::Value`, always valid JSON at any size. So there is no threshold to measure.
      **What is real, and is filed separately**: `agent-loop.flux` does `$intent_kind = $intent.kind`
      with no guard, so *any* future or provider-side path that leaves a stage result non-JSON dies
      with `field access `.kind` … of a string` — a diagnostic that names flux-lang instead of the
      stage that failed. That diagnostic defect is worth its own story; it is not fixed here.
- [x] Grep for other tests that read live repository or machine state — `git`, `$HOME`, the network,
      the clock — and list them. If a second one exists, this story's fix should generalise or the
      story should say why it cannot.
      → Census in the implementor's report. Exactly one other test *executes git against the
      developer's checkout*: `crates/flux-app/tests/strict_review_journey.rs`
      (`journey_and_direct_flow_produce_the_same_review_report`), same flow, same defect. **The fix
      cannot generalise there today**: the pin needs `ToolRegistry::replace_from` after built-in
      assembly, and `App` offers no such seam — `extra_tools` are `try_extend`-ed *after*
      `try_register_builtins` (`crates/flux-app/src/app.rs:872,887`), so a same-name op is a
      collision, not a substitution. Adding a replacement seam to `App` is a flux-app API change and
      belongs in its own story.
- [x] Full gate green in both workspaces, and specifically green with a deliberately dirty tree.

## Progress

- 2026-07-31 — C-319 implemented test-only, on `impl/C-319`.
  - `crates/flux-sdk/tests/strict_review.rs`: pinned `git_status`/`git_diff` to checked-in fixtures
    via `FlowClient::try_register_pack` + `ToolRegistry::replace_from`; pointed `files` at a
    checked-in fixture source; added the prompt-capturing regression test.
  - `crates/flux-sdk/tests/fixtures/strict_review/{status.txt,diff.patch,subject.rs}` added. The
    diff fixture is 34 609 chars — deliberately ~1.7× `tool_output_cap()`'s 20 000-char default — and
    the test asserts it stays over the cap, so the over-cap path is exercised on every run instead of
    only on a machine that happens to carry a large uncommitted diff.
  - Side effect worth having: the suite no longer shells out to git, so it dropped from ~7.6 s
    (against a 17 MB working diff) to ~0.5 s.
  - Left for a resuming agent: (1) the `App` replacement seam that would let
    `crates/flux-app/tests/strict_review_journey.rs` be pinned the same way; (2) the `agent-loop.flux`
    `$intent.kind` diagnostic described above.

## Notes

- Found by C-304's implementor (2026-07-31) while implementing an unrelated story; recorded because a
  test that ambushes implementors costs far more than its own runtime.
- Related: [C-307](C-307-app-run-ignores-limits.md) established the principle this violates — a
  regression gate's verdict must depend only on its fixture, which is why `flux test`'s offline
  client deliberately does not read the local `[limits]` table.
