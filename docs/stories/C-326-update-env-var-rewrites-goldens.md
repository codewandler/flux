---
id: C-326
title: "An ambient `UPDATE` env var turns three golden assertions into golden rewrites that pass vacuously"
pillar: Core
status: in-progress
priority: 3
areas: [flux-lang, flux-plugin-protocol]
note: "found by C-319's live-state census and confirmed by its reviewer — skill_in_sync, website_in_sync and wire_contract all gate on env::var(\"UPDATE\").is_ok(), so PRESENCE arms it: UPDATE=0 or even UPDATE= rewrites the goldens and the test passes having compared nothing"
---

# An ambient `UPDATE` rewrites the goldens instead of checking them

## Goal

Stop three in-sync guards from silently becoming no-ops on a developer machine that happens to have
`UPDATE` exported.

`crates/flux-lang/tests/skill_in_sync.rs:20`, `crates/flux-lang/tests/website_in_sync.rs:36` and
`crates/flux-plugin-protocol/tests/wire_contract.rs:36` all gate on
`std::env::var("UPDATE").is_ok()`. That tests **presence, not value** — `UPDATE=0`, or even an empty
`UPDATE=`, arms it. Each then takes a `write(...); return;` branch *before* its `assert_eq!`
(`skill_in_sync.rs:28-35`, `website_in_sync.rs:94-97`, `wire_contract.rs:36-42`), so the golden file
is overwritten with whatever the code currently produces and the test reports **ok** having compared
nothing.

This is worse in kind than C-319, which is what surfaced it. C-319's test *failed* on a dirty tree —
loudly, expensively, but visibly. This one **goes green while proving nothing**, which is the failure
mode with no symptom at all. A drifted node-kind table, a stale website reference or a broken
plugin wire contract would all be silently blessed, and the diff would show the golden updating as
though someone meant it.

`UPDATE` is a common enough variable name that an unrelated tool or a shell profile can export it.
The regeneration workflow itself is legitimate and documented (`UPDATE=1 cargo test …`) — the defect
is that arming it is indistinguishable from ambient environment noise.

## Acceptance

- [x] **Failing-first**: a test showing that with `UPDATE` set to something that is obviously not an
      opt-in (`UPDATE=0`, or empty) a drifted golden is rewritten and the guard reports success.
      Reproduce it for at least one of the three, and say whether the other two share the mechanism
      exactly.
      → reproduced at the merge base for **all three** (see Progress); pinned going forward by
      `crates/flux-lang/tests/golden_arming.rs` +
      `crates/flux-plugin-protocol/tests/golden_arming.rs`.
- [x] Arming regeneration requires an explicit, intentional value. Decide the spelling — `UPDATE=1`
      only, a differently-named variable, or a `--` test argument — and say what you rejected. The
      documented workflow in `AGENTS.md` and any `docs/` references must be updated in the same
      commit if the spelling changes.
      → `FLUX_UPDATE_GOLDEN=1`, matched exactly; unset/empty checks; anything else is refused rather
      than guessed at (`tests/support/golden_mode.rs::mode_from`). Docs updated in this commit.
- [x] **A regeneration run cannot report success as though it had verified something.** Whatever the
      arming mechanism, a run that *writes* should be distinguishable in its output from a run that
      *checked* — the current failure is not just how it is armed but that the two outcomes are
      indistinguishable afterwards.
      → `golden_mode::rewrote()` writes the file and then **fails** with `REGENERATED <path> — this
      run wrote the golden and verified nothing`. A regenerating run is red; a checking run is green.
- [x] All three sites are fixed together. They share one mechanism and a partial fix leaves the same
      trap in the remaining ones.
      → `skill_in_sync.rs`, `website_in_sync.rs` (both via `tests/support/golden_mode.rs`) and
      `wire_contract.rs` (its own copy — the protocol crate ships on the independent 1.x line and
      must not depend on flux-lang for a test helper).
- [x] Grep for any other `env::var(...).is_ok()` presence-check that changes test behaviour, and list
      what you find. Presence-vs-value is the actual bug class here; these three may not be the only
      instances.
      → census in Progress: 5 further presence-checks, all of them *widening* (opt-in to run an extra
      live-smoke test, or a colour/mock switch), none silencing an assertion. Left alone.
- [x] Full gate green in both workspaces.

## Notes

- Found by [C-319](C-319-strict-review-test-depends-on-tree-dirtiness.md)'s mandated census of tests
  reading live machine state, and independently confirmed at file:line by its reviewer, which noted
  the `write(...); return;` precedes the assertion in all three.
- This is a close relative of a scar this project already carries: a guard tested against its own
  assumptions. Here the guard is not tested against anything at all when the variable is present.
- ⚠ Note the coordinator ran `UPDATE=1 cargo test -p codewandler-flux-lang --test website_in_sync`
  repeatedly during this wave to regenerate the customer-changelog mirror — that is the legitimate
  documented use, and it is *why* the trap is easy to miss: the mechanism works exactly as intended
  right up until the variable is set by something else.

## Progress

**Reproduced at the merge base** (`0df177c2`), before any change:

- `skill_in_sync`: appended a comment to `crates/flux-lang/skill/SKILL.md`. Unarmed →
  `skill_artifact_is_in_sync ... FAILED` (the guard works). Then `UPDATE=0` → `test result: ok. 3
  passed`, and `git diff` on the golden was **empty** — the drift had been silently overwritten.
- `wire_contract`: renamed `name` in `tests/golden/manifest.json`. Unarmed →
  `manifest_wire_surface_is_pinned ... FAILED`. Then an **empty** `UPDATE=` → that test passed and
  the drift was gone.
- `website_in_sync`: corrupted a row inside the `generated:node-kinds` block of
  `website/docs/language/node-reference.md`. `UPDATE=0` → `test result: ok. 3 passed`, drift gone.

So yes — all three share the mechanism exactly: the same `env::var("UPDATE").is_ok()` presence test
followed by `write(...); return;` ahead of the assertion.

**Spelling: `FLUX_UPDATE_GOLDEN=1`, matched exactly.**

- Rejected *keep `UPDATE`, require `=1`*: cheapest (no doc churn) but leaves a bare, un-namespaced
  name that any tool or profile can export. `UPDATE=1` is not a rare ambient value, and the story's
  own diagnosis is that the name is the problem.
- Rejected *a `--` test argument*: libtest passes unknown args through as a name filter, so
  `-- --regenerate` would silently select zero tests and print `ok. 0 passed` — the same vacuous
  green in a new costume. It also cannot be expressed in `scripts/cut-release.sh` as cleanly.
- Rejected *honouring the old `UPDATE` as a deprecated alias*: that keeps the ambient-collision
  surface it exists to remove. Clean cutover; stale muscle memory now yields a normal drift failure
  whose message names the new spelling.
- Rejected *panicking when the legacy `UPDATE` is seen*: that would turn an ambient variable into a
  red suite — C-319's loud-but-wrong failure class, traded for this one. `UPDATE` is simply not read.

**Distinguishable output.** `golden_mode::rewrote()` panics after writing, so libtest reports the
regenerating run as FAILED with `REGENERATED <path> — this run wrote the golden and verified
nothing`. Because libtest catches per test, every guard in the binary still writes its own file
first, so the regeneration workflow is intact — it is just no longer green. `scripts/cut-release.sh`
was adapted: it discards the armed run's status and then re-runs the same test unarmed as the real
check, which is stricter than before (a silently-unwritten mirror used to reach the release commit).

**Census — every other `env::var(..).is_ok()`/`is_err()`/`var_os(..).is_some()` that changes test or
runtime behaviour.** None of them can silence an assertion; all either *add* work or flip
presentation:

| site | effect | verdict |
|---|---|---|
| `crates/flux-web/src/browser.rs:1663` | `FLUX_LIVE_BROWSER_SMOKE` unset ⇒ `return` early from a live-browser smoke test | opt-*in* to more checking; presence is the right question. Leave. |
| `crates/flux-system/src/lib.rs:4230` | `FLUX_LIVE_SANDBOX_SMOKE`, same shape | same. Leave. |
| `crates/flux-lang/tests/wasm_parity.rs:51` | `FLUX_PORTABLE_WASM_REQUIRED` unset ⇒ skip instead of fail when the wasm artifact is absent | presence = "this environment must have wasm"; setting it only makes the test stricter. Leave. |
| `crates/flux-cli/src/execution.rs:1932`, `:2082` | `FLUX_MOCK_HANG` / `FLUX_MOCK_TOOL` select mock behaviour in test-only paths | test-fixture switches, not assertions. Leave. |
| `crates/flux-cli/src/main.rs:4994`, `:5091` | asserts `FLUX_SANDBOX_NET` is *absent* — presence is the property under test | correct as written. Leave. |
| `NO_COLOR` (`flux-channels`, `flux-cli`, `flux-tui` ×2, `flux-markdown`) | disables colour | the NO_COLOR spec defines presence as the trigger. Leave. |

The distinguishing property: C-326's three took a `write(...); return;` **before** an `assert_eq!`,
so arming them removed a check. Nothing else in the census does that.
