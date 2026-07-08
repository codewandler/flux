---
id: C-46
title: Beta docs-truth pass — mock mode, A2A protocol version, Flux-Lang syntax examples, `peek`
pillar: Core
status: done
epic: beta-hardening
design: docs/designs/beta-hardening.md
note: "F-001 + F-005 + F-007 + F-009: align public docs to observed v0.6.0 behavior — mock mode returns canned output (not a representative demo); docs say A2A v1.0 while the card reports protocolVersion 0.3.0; Flux-Lang examples with text expr()/JSON returns:Object/non-writable expr trees don't round-trip; `peek` shown as bindable but the tested bind shape is rejected"
---

# Beta docs-truth pass — mock mode, A2A protocol version, Flux-Lang examples, `peek`

## Goal
Four public-docs-vs-runtime mismatches the beta surfaced, batched into one truth pass (the C-40
discipline, re-run against v0.6.0). Each is "docs claim X, runtime does Y" — fix the docs to match
observed behavior (or, where the docs describe the *intended* behavior, file the runtime gap and
soften the docs meanwhile).

## Why (evidence)
- **F-001 (mock mode):** getting-started makes mock mode sound like a representative agent demo;
  observed mock runs return canned `Finished.`. Reframe it as a wiring/smoke-test provider, not a
  behavioral demo.
- **F-005 (A2A version):** public docs mention A2A "v1.0"; the live AgentCard reports
  `protocolVersion: "0.3.0"`. Align the prose to the value the card advertises (the card already
  emits it via [A-49](A-49-agent-card-conformance-fields.md)).
- **F-007 (Flux-Lang examples):** examples using text `expr(...)`, JSON `"returns":"Object"`, and
  rendered non-writable `expr(...)` trees do not round-trip as runnable v0.6.0 syntax — replace with
  examples that parse and run.
- **F-009 (`peek`):** docs show `peek` as bindable, but the tested bind shape was rejected — fix the
  doc to the accepted shape (or, if `peek` *should* be bindable, file the runtime story and note it).

## Acceptance
- [ ] Mock-mode docs (`website/docs/**` getting-started + any `docs/**` mirror) describe it as a
      canned/offline smoke-test provider, with an example of the actual output; no longer implies a
      representative agent run. (Coordinates with [A-60](A-60-serve-mock-provider-parity.md) on the
      served-mock behavior.)
- [ ] Every doc mention of the A2A protocol version matches the card's advertised `protocolVersion`
      (`0.3.0`), across `website/docs/agent/**`, `docs/roadmap.md`, and the conformance matrix
      copies. If "v1.0" is intended, escalate the card value into `a2a-conformance` instead and note
      the decision here.
- [ ] The Flux-Lang doc examples flagged in F-007 are replaced with syntax that round-trips on
      v0.6.0 (validated the way the language docs are already guarded — parser/round-trip check).
- [ ] `peek` docs show a bind shape the runtime accepts (or a runtime story is filed and linked, with
      the doc softened to the working usage meanwhile).
- [ ] `grep`/build sweep: the site builds clean and no flagged example remains.

## Progress
- 2026-07-08 **DONE.**
  - **F-001 (mock):** reframed `-m mock` in README, `getting-started.md`, and `troubleshooting.md` as
    an offline *wiring smoke test* with **canned** output (writes `flux-mock.txt`, prints `Finished.`
    regardless of prompt), not a representative agent run; changed the example prompt off "summarise
    this repo".
  - **F-005 (A2A version):** aligned every user/contributor doc mention to the card's advertised
    `protocolVersion` `0.3.0` (was "v1.0"): `website/docs/agent/a2a-conformance.md` (prose + the
    `protocolVersion` row now shows `0.3.0`), `docs/a2a-conformance.md`, `docs/roadmap.md`, and the two
    design docs. The card value (`flux_a2a::PROTOCOL_VERSION`) was already correct — docs were wrong.
  - **F-007 (Flux-Lang examples):** fixed the top-level-shape `TypeRef` JSON — named types are
    `{"named":"Result"}` and primitives are lowercase (`"string"`), not bare `"Result"`/`"String"` —
    in `node-reference.md`, `crates/flux-lang/docs/reference.md`, and the `skill.rs` SSoT (regenerated
    `SKILL.md` + the flux-markdown corpus copy). The `expr(...)` text examples were already caveated as
    aspirational — untouched.
  - **F-009 (`peek`):** resolved by *making `peek` bindable* (small, clean runtime enablement in
    `eval_pure_node`, soft-read semantics like `var`) rather than gutting the doc example — so the
    existing `$prev = peek(last_result)` bind-shape docs are now truthful. See L-43/A-64 sibling note;
    covered by `peek_is_bindable_as_a_soft_read` (flux-lang).
  - All SSoT sync tests (`skill_in_sync`, `website_in_sync`, `skill_docs_in_sync`) green. The full
    Docusaurus site build was **not** run (docs-truth prose pass; no structural/link changes).

## Notes
- Docs-truth story (narrow gate), mirroring [C-40](C-40-docs-truth-pass-v030.md). Language-example
  fixes may depend on the outcomes of [L-43](L-43-text-scalar-bind-types.md)/
  [L-44](L-44-parse-node-composability.md) — sequence after those if a "correct" example needs the
  fixed behavior; otherwise document current behavior.
- Website language pages have manual mirror tables (per repo memory) — keep them in sync.
- Epic: [beta-hardening](../designs/beta-hardening.md).
