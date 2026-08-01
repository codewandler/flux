---
id: C-434
title: "The worked recipe — explore a real site, freeze it, run the frozen script in CI"
pillar: Core
status: backlog
design: docs/designs/explore-then-freeze.md
epic: explore-then-freeze
areas: [examples, docs]
note: "this epic's proof and a member of the flux-recipes family (C-427's contract applies). Filed under explore-then-freeze so it is not orphaned if that epic re-sequences. ⚠ Needs a target that can be committed — a public demo app, not a production login"
---

# The whole loop, on something real

## Goal

One page and one runnable artifact showing the loop end to end: an agent explores a site, one command
freezes the path that worked, and the frozen script runs in CI with no model.

## Acceptance

- [ ] Satisfies the recipe contract from [C-427](C-427-the-recipe-contract.md): a real task, runnable
      from a clean checkout, stating which guarantee it demonstrates and the command that verifies it.
- [ ] ⚠ **The target is committable.** A public demo application or a fixture served locally — never a
      production login, and never a real account's credentials. The recipe must be runnable by a
      stranger, which rules out anything requiring our secrets.
- [ ] The frozen script runs **without a model** in CI. The browser stack is transport-agnostic below
      the `ops` boundary and tests already drive a scripted CDP fake over an in-memory duplex, so **no
      Chrome in CI** is achievable — use that path rather than adding a browser to the runners.
- [ ] Shows the exploration *and* the frozen result side by side. The contrast is the entire argument:
      one is a session with backtracking, the other is a committable script. A page showing only the
      result demonstrates nothing that a normal test framework does not.
- [ ] ⚠ Uses only [C-432](C-432-browser-credentials-never-come-from-the-prompt.md)'s credential path,
      with no prompt-embedded password anywhere — including in prose. Sample strings get copied.
- [ ] Honest about limits: what happens when the UI changes, what a locator cannot survive
      (localization, per [C-431](C-431-durable-locators.md)), and that the exploration itself is not
      deterministic. A recipe that hides these gets found out by the first person who tries it on their
      own app.
- [ ] Full gate green, including whichever sweep the recipe contract puts it under.

## Notes

- **Blocked on C-430 + C-431 + C-432** at minimum; [C-433](C-433-a-frozen-script-asserts.md) should
  land first too, or the recipe demonstrates a click-runner rather than a test.
- Cross-epic: this belongs to the [flux-recipes](../designs/flux-recipes.md) family and is exactly the
  kind of entry [C-429](C-429-the-recipes-surface-and-positioning.md)'s page wants — a guarantee plus
  the command that verifies it. Filed here so it is not orphaned if that epic re-sequences.
- ⚠ This is the single most demo-able thing in the epic, which is precisely why it must not ship first.
  A polished page over a distiller that emits brittle scripts would be actively harmful — the people
  most impressed are the ones who would adopt it.
- The screencast epic ([session-screencast](../designs/session-screencast.md)) could render this loop
  as a cast, which would be its strongest possible asset. Not a dependency in either direction.

## Progress

- Filed 2026-08-01 with the explore-then-freeze epic.
