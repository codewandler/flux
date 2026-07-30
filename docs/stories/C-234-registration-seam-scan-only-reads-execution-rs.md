---
id: C-234
title: "The catalog-coherence registration-seam scan only reads `execution.rs`, so a pack registered from `app_cmd.rs` escapes the census"
pillar: Core
status: done
priority: 13
epic: security-assurance
design: docs/designs/security-assurance.md
areas: [flux-cli]
note: "filed from A-131's implementor report — the board seam was caught only because build_datasources' doc comment happens to name try_register_work_board; that is a textual accident, not a property"
---

# The catalog-coherence registration-seam scan only reads `execution.rs`, so a pack registered from `app_cmd.rs` escapes the census

## Goal
`every_registration_seam_in_the_cli_assembly_is_classified`
(`crates/flux-cli/src/catalog_coherence.rs:279`) is C-208's drift guard: the production catalog census
is assembled by hand, so it "goes stale the moment a pack is added to `build_agent_with` and not here,
and it goes stale *silently* — the gate keeps passing while covering less" (`:250-252`). The guard's
answer is to read the source and force every registration seam to be classified as covered or
excluded.

It reads exactly one file:

```rust
std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/execution.rs"),
```
(`catalog_coherence.rs:325-327`)

Its own doc comment admits the limit (`:269-273`): "Only `execution.rs` is scanned. A pack registered
into the agent's catalog from another `flux-cli` module would not be seen", followed by the reassurance
that `app_cmd.rs`'s `assemble_integrations` "reaches the catalog only through the `source` label
classified below, so it is covered today — but that is a fact about the current call graph, not
something this test enforces."

A-131 walked into precisely that. The board registration loop it adds lives in
`crates/flux-cli/src/app_cmd.rs` — the module that already calls `build_datasources`
(`app_cmd.rs:496`) — not in `execution.rs`. Its new seam was caught by this guard **only** because
`build_datasources`' doc comment in `execution.rs` happens to mention `try_register_work_board`, and
the scan matches source text including comments. A guard that fires on a doc comment is not
enforcing a property; it is getting lucky. Move the word and the seam disappears from the census with
no test going red — and a future pack registered from `app_cmd.rs` under a *fresh* source label
(so the `try_register_from` label classification does not catch it either) escapes entirely.

Close the documented hole rather than restating it.

## Acceptance
- [x] The seam scan covers every `flux-cli` module that can reach the agent's catalog, not just
      `src/execution.rs`. The obvious shape is to walk `crates/flux-cli/src/` — `app_cmd.rs` and
      `main.rs` at minimum — but whichever set is chosen must be derived, not hand-listed, so a new
      module cannot be born outside the scan.
- [x] Failing-first test: a registration call added to `crates/flux-cli/src/app_cmd.rs` under a
      **fresh** source label fails the guard as unclassified. On today's tree it passes — that is the
      bug. (A test fixture or a scan-level unit test is fine; do not leave a stray production
      registration behind to make it fail.)
- [x] The doc-comment coincidence is removed as a load-bearing mechanism: the scan must not depend on
      a seam name appearing in prose. Either restrict matching to code (the existing production-body
      filter at `:328` already establishes that the scan reasons about which text counts), or state
      plainly in the doc comment that comments are matched deliberately and why that is sound.
- [x] The "What this does *not* catch" section (`:264-277`) is rewritten to match the widened scan.
      The remaining hole — a new registration reusing an already-classified source label (`:274-276`)
      — stays documented; it is out of scope here, and narrowing scope silently is the failure mode
      this whole guard exists to prevent.
- [x] The non-vacuity floor is preserved and re-tuned for the widened scan: the existing assertion
      that a suspiciously small number of "seam(s) and source label(s)" means "the scan probably
      stopped early" (`:373`) must still be able to catch a scan that silently read nothing.
- [x] Direct `try_register` calls are classified by their registered tool/pack identity, so a fresh
      source cannot inherit approval from the method name alone.
- [x] Standard gate green in both workspaces (root + `plugins/`), `cargo fmt --check` included.

## Progress
- 2026-07-30 — replaced the `execution.rs` substring scan with a recursively derived `flux-cli/src`
  Rust-AST census. It excludes `#[cfg(test)]` modules structurally, ignores comments and string
  contents, classifies both method seams and generic-registration source labels, and asserts floors
  for modules, calls, seams, and labels. Fixtures prove an `app_cmd.rs` fresh label fails and prose
  cannot satisfy the guard. The only documented residual limit is deliberate reuse of an existing
  audit label.
- 2026-07-30 — promoted to `ready` as an existing exact-match child of the C-255 remediation epic;
  its original security-assurance ownership and acceptance criteria remain unchanged.
- 2026-07-30 — closure review removed the blanket approval for direct `try_register` calls. The AST
  census now extracts the registered tool/pack identity at that seam, classifies the two existing
  direct identities explicitly, and rejects a fixture that introduces `FreshTool` through the same
  previously approved method name.

## Notes
- Filed 2026-07-29 from the fleet-coordinator integration run, out of **A-131's implementor report**.
  The evidence as given: `catalog_coherence.rs`'s own doc comment admits it scans only
  `execution.rs`, but the board registration loop actually lives in
  `crates/flux-cli/src/app_cmd.rs`; A-131's board seam was caught only because `build_datasources`'
  doc comment happens to mention `try_register_work_board` — a textual accident, not a property — and
  a future pack registered from `app_cmd.rs` under a fresh source label would escape the census
  entirely. Re-verified against `main` at base `9721daca`.
- This is a guard-of-a-guard story: the census in `catalog_coherence.rs` is what C-208 used to find
  22 violations across 19 operations, including two (`explore`, `grade`) that "nobody had found
  because nothing had ever walked this catalog" (`:16-18`). Its value is entirely a function of the
  drift guard staying honest about coverage.
- Read `docs/designs/security-assurance.md` before changing declarations this census covers — the
  posture decisions it gates are recorded there, not in the test.
- Sibling of C-233 (the published risk column skips every non-built-in op): same class — a drift
  guard narrower than what it is trusted to cover. They touch different files and can be worked in
  parallel, though both end up reading `catalog_coherence.rs`'s census.
