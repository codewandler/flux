---
id: C-194
title: "Enforce the no-direct-IO invariant mechanically — tools must not reach the filesystem behind flux-system"
pillar: Core
status: ready
priority: 4
epic: security-assurance
design: docs/designs/security-assurance.md
note: "REVIEW — architecture.md:169 states 'tools never touch std::fs/std::process directly' and NOTHING checks it; the layering lint sees dependency direction, validate_authority_contracts sees declaration coherence, neither sees fidelity to execute — which is exactly how C-192 shipped"
---

# Enforce the no-direct-IO invariant mechanically — tools must not reach the filesystem behind flux-system

## Goal
flux's safety claim is architectural, but its enforcement is per-tool and manual, in a registry the
project intends to grow. `docs/architecture.md:169` states the invariant; nothing in the tree checks
it. [C-192](C-192-sqlite-query-vacuum-into-escape.md) is what that costs. Convert the invariant from
a review-discipline promise into a gate, so the next violation fails CI at authoring time instead of
surfacing in the next adversarial review.

## Acceptance
- [ ] A lint or test rejects `std::fs`, `std::process::Command`, `tokio::fs`, `tokio::process` and
      direct database/socket opens (e.g. `rusqlite::Connection::open*`) in model-facing tool crates
      outside `#[cfg(test)]`.
- [ ] Scope decided and recorded: at minimum `flux-tools`, `flux-web`, `flux-capabilities`. Whether
      it extends to the plugin host and `flux-eval` is a judgement call the change should state.
- [ ] It runs in CI as a **named** step, the way the layering lint does
      (`.github/workflows/ci.yml:54`), so a regression is an obvious named failure rather than a
      buried assertion.
- [ ] Failing-first demonstration: the lint flags `crates/flux-tools/src/extra.rs:341` (the direct
      `rusqlite` open) against the pre-C-192 tree. If C-192 has already landed, the demonstration is
      a temporary reintroduction in a test fixture — not a weakened guard in shipped code.
- [ ] Any legitimate exception is an explicit, greppable allow-annotation carrying a reason, not an
      unlisted omission from the lint's scope.
- [ ] `docs/architecture.md:169` gains a pointer to the enforcing check, so the invariant and its
      proof are findable together.

## Progress
- (not started)

## Notes
- **Verified against the tree at `0.33.1` (f8e90d7).** Source review:
  [`reviews/2026-07-29-envelope-integrity.md`](../../reviews/2026-07-29-envelope-integrity.md),
  finding 3.
- What exists and what it actually covers:
  - **`flux-codegate`** (`.github/workflows/ci.yml:54`) is a crate-*dependency*-direction lint. It
    cannot see `std::fs` use inside an allowed dependency edge.
  - **`ToolRegistry::validate_authority_contracts`** (`crates/flux-runtime/src/lib.rs:1583-1596`)
    calls `authority_requirements(&json!({}), …)` per tool and checks it returns `Ok`. That is
    *internal coherence* of a declaration on empty input. It has no way to know whether the
    declaration is faithful to what `execute` does.
- **Grepped for and not found** anywhere under `crates/`: any test or lint named for or asserting
  this invariant (`no_direct_io`, `no_std_fs`, "never touch std::fs", "all IO goes through") — zero
  hits outside the architecture doc's own prose. `grep` returning nothing is the evidence here.
- The sibling invariant at `architecture.md:170` — *"nothing calls a tool's `execute` directly in
  prod"* — **does** hold in the tree: every `.execute(` hit outside `flux-runtime` is inside a
  `#[cfg(test)]` module. Worth pinning with the same lint while it is being written, so it stays
  true for the same reason rather than by luck.
- **Division of labour in this epic.** [C-191](C-191-toolspec-invariant-test.md) checks a `ToolSpec`
  is internally coherent; this story checks the implementation is faithful to the spec. C-192's tool
  had a *coherent* spec and still bypassed guarded IO — the two checks catch disjoint failure modes
  and neither substitutes for the other.
