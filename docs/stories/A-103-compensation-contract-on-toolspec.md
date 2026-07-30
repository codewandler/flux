---
id: A-103
title: "The Compensation contract on ToolSpec — every mutating op declares how it is reversed"
pillar: Agent
status: ready
priority: 14
epic: transactional-turns
design: docs/designs/transactional-turns.md
note: "Inverse | Snapshot | NotNeeded | None{why}; a registry-walk test fails on any mutating built-in with no declaration, which is what stops the contract rotting as ops are added"
---

# The Compensation contract on ToolSpec — every mutating op declares how it is reversed

## Goal
Give every operation a declared answer to "how is this undone?" as a sibling of the existing
`effects: Vec<Effect>` on `ToolSpec` (`flux-spec/src/lib.rs:267`). Declaration is static and
therefore available at approval time, which is what makes the irreversibility risk signal (A-106)
possible even though the concrete reverse action can only be materialized at execution time.

## Acceptance
- [ ] `Compensation` enum in `flux-spec`: `Inverse { op }`, `Snapshot { capture, op }`,
      `NotNeeded`, `None { why }`. `NotNeeded` is the default for read-only ops so they need no
      annotation.
- [ ] `ToolSpec::with_compensation` builder, mirroring `with_effects`.
- [ ] Every mutating built-in op declares one. **Failing-first test**: a registry walk that fails
      on any op whose effects include a non-`Read` effect and whose compensation is unset — it must
      fail before the declarations are added, and it is the mechanism that keeps future ops honest.
- [ ] `None { why }` is a first-class, documented answer — `send_external`, `money`, and `bash`
      declare it with a real reason string, not a placeholder. A test asserts `bash` is `None`
      (flux cannot know what arbitrary argv did).
- [ ] The `why` string is surfaced verbatim by the consumers in A-105/A-106 — assert it is not
      `&'static str`-erased at any seam.
- [ ] No behaviour change: nothing reads `Compensation` yet.

## Progress
- Not started.

## Notes
- Design: [transactional-turns.md](../designs/transactional-turns.md).
- ⚠ **Adding `Compensation` + `with_compensation` changes `flux-spec`'s public API, and `flux-spec`
  is itself on the independent protocol line (`1.x`)** — separate from the wire-protocol caveat
  below, which is about not touching `flux-plugin-protocol`. The addition is additive, so it needs a
  MINOR bump *only if* the current version is already released; check first, because as of C-210 the
  crate sits at an unreleased `1.2.0` and a second bump would strand that version. Run
  `./scripts/check-crate-versions.sh` before pushing — CI is otherwise the only thing that catches
  this, and it has bitten twice.
- The registry-walk test in the Acceptance is a sibling of `flux_spec::metadata_violations`
  (C-191/C-208/C-210). Consider whether it belongs alongside the existing coherence invariants
  rather than as a separate walk — one place that answers "is this op's declaration honest?" is
  easier to keep true than two.
- Lives in `flux-spec` because plugin manifests will eventually want to declare it too (it belongs
  to the same vocabulary as `semantic_effects`); do **not** add it to the plugin wire protocol in
  this story — the protocol crates are on an independent 1.x line and a wire change is its own
  decision.
- Keep the C-184 vocabulary invariant in mind: `Compensation` names a *mechanism*, never a domain.
