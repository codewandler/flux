---
id: C-431
title: "`e<N>` refs mean nothing in a fresh session — freezing must re-anchor them to role and name"
pillar: Core
status: ready
priority: 12
design: docs/designs/explore-then-freeze.md
epic: explore-then-freeze
areas: [flux-web, flux-flow]
note: "⚠ VERIFIED, not suspected: RefMap (crates/flux-web/src/digest.rs:53-72) keys on backendDOMNodeId and assigns `next += 1` in first-encounter order WITHIN one live session. `e17` is stable while exploring and meaningless in a new session. The fix material is in the same data — every ref carries an AX role and name (digest.rs:180)"
---

# The ref that is stable for the agent and worthless for the script

## Goal

A frozen script locates a control the way a user would — by what it *is* — so it survives a DOM change
that renumbers everything.

## The finding, verified in code

`RefMap` (`crates/flux-web/src/digest.rs:53-72`):

```rust
pub struct RefMap { by_backend: HashMap<i64, u32>, next: u32, alive: HashSet<i64> }

fn ref_for(&mut self, backend: i64) -> u32 {
    if let Some(&n) = self.by_backend.get(&backend) { return n; }
    self.next += 1;  self.by_backend.insert(backend, self.next);  self.next
}
```

Refs are keyed to `backendDOMNodeId` and handed out in **first-encounter order within one live
session**. That makes `e17` genuinely stable *while the agent explores* — exactly what it was designed
for — and **meaningless in a fresh session**, where numbering restarts and depends on the navigation
path taken.

⚠ **A distiller that emits `e17` produces a script that breaks on the next deploy.** That is precisely
what made a generation of record-replay e2e tools disposable, and it would make this feature a
liability rather than a differentiator.

**The fix is in the same data.** The digest is built purely from `Accessibility.getFullAXTree` — *"what
a screen reader sees — roles, names, states"* — and each entry renders its role and name
(`digest.rs:180`). `e17` can be re-anchored to `role=button, name="Sign in"`: a locator that survives a
refactor because it is how a screen reader, and a person, finds the control.

## Acceptance

- [ ] **Failing-first**: a test that resolves a ref, mutates the AX tree so numbering shifts, and
      asserts the re-anchored locator still finds the same control while the raw ref does not — failing
      at the merge base.
- [ ] Freezing emits role+name locators. The raw ref may be retained as a comment for provenance; it
      must not be what the script matches on.
- [ ] ⚠ **Ambiguity fails loudly.** Two controls with the same role and name is the common case (two
      "Submit" buttons, a repeated "Edit" in a table). **A script that silently clicks the wrong one is
      worse than a script that fails** — a wrong-target click can mutate real state. Define the
      disambiguation (nearest labelled ancestor, ordinal within a container, something else), and pin
      the ambiguous case with a test asserting a *loud* failure rather than a guess.
- [ ] ⚠ **A missing locator fails loudly too**, naming what it looked for. "Element not found" with no
      role/name is what makes flaky e2e suites unmaintainable.
- [ ] The unlabelled case is handled honestly. `is_fallback_clickable` (`digest.rs:134-144`) already
      surfaces focusable named `generic` nodes as buttons — decide what a locator means for a control
      whose only identity is that heuristic, and say so.
- [ ] The re-anchoring is **pure** over a captured AX payload, matching how `build_digest` is already
      testable without Chrome — the property that keeps this in CI.
- [ ] Full gate green.

## Notes

- Pairs with [C-430](C-430-distil-an-exploration-into-a-flow.md); the distiller cannot emit a durable
  script without this, and this has no consumer without the distiller. Either order works if the
  interface is agreed; C-430's Acceptance covers what it does if this has not landed.
- ⚠ Do not extend to CSS/XPath selectors as a fallback without a decision. They are more expressive and
  far more brittle, and reaching for one is how the semantic locator quietly stops being the contract.
- The digest's ordering is document order, described in its own header as *"replay/`flux diff`
  friendly"* — worth reading before choosing an ordinal disambiguation scheme, since one already exists.
- Accessible names are user-visible text, so they change with **localization**. A script frozen against
  a German UI will not run against English. Decide whether that is documented, handled, or out of scope
  — but do not discover it in production.

## Progress

- Filed 2026-08-01 with the explore-then-freeze epic, from a read of `RefMap`.
