---
id: C-326
title: "An ambient `UPDATE` env var turns three golden assertions into golden rewrites that pass vacuously"
pillar: Core
status: ready
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

- [ ] **Failing-first**: a test showing that with `UPDATE` set to something that is obviously not an
      opt-in (`UPDATE=0`, or empty) a drifted golden is rewritten and the guard reports success.
      Reproduce it for at least one of the three, and say whether the other two share the mechanism
      exactly.
- [ ] Arming regeneration requires an explicit, intentional value. Decide the spelling — `UPDATE=1`
      only, a differently-named variable, or a `--` test argument — and say what you rejected. The
      documented workflow in `AGENTS.md` and any `docs/` references must be updated in the same
      commit if the spelling changes.
- [ ] **A regeneration run cannot report success as though it had verified something.** Whatever the
      arming mechanism, a run that *writes* should be distinguishable in its output from a run that
      *checked* — the current failure is not just how it is armed but that the two outcomes are
      indistinguishable afterwards.
- [ ] All three sites are fixed together. They share one mechanism and a partial fix leaves the same
      trap in the remaining ones.
- [ ] Grep for any other `env::var(...).is_ok()` presence-check that changes test behaviour, and list
      what you find. Presence-vs-value is the actual bug class here; these three may not be the only
      instances.
- [ ] Full gate green in both workspaces.

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
