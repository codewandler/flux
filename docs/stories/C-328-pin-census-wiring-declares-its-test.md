---
id: C-328
title: "A wiring line declares the test that observes it — the pin census"
pillar: Core
status: in-progress
priority: 2
areas: [flux-codegate, flux-cli]
design: docs/designs/unobserved-wiring.md
note: "the keystone for this repo's #1 recurring defect — 19 stories have found production wiring that no test observes, and each was answered by hand-building a NEW bespoke guard. Ships RED on main with C-314's two known-unpinned sites as the proof, and closes C-314 in the same story"
---

# A wiring line declares the test that observes it

## Goal

Nineteen stories have found a production wiring line that is correct and that **no test observes** —
found only by deleting the line and seeing nothing change colour. C-305 is the sharpest recent one:
deleting two `flux-tui` wiring lines left **474 tests green** while no model pane could ever reach a
terminal. C-314 is live today: deleting **both** `[limits]` wirings leaves the entire `flux-cli`
suite green.

**The debt is not the bug — it is that each instance is answered by authoring a new guard.** There
are ~10 now, each with its own mechanism and its own anti-vacuity proof. Guard #11 is always
someone's next story.

Introduce **one** mechanism instead:

> A wiring line declares, in-source, the test that dies without it. A census proves the declaration
> exists and resolves.

This story is **Half A only** — the static census. The dynamic runner that proves each pin is
*honest* is C-329.

## Why not `cargo-mutants` — decided, with evidence

Its operator set is *body replacement* and *binary-operator swaps*. It does **not** delete
statements and does **not** drop a call from a method chain. Both C-314 sites are builder chains
inside functions returning non-`Default` types (`Result<Client>`), so the only mutant available is an
unviable whole-body replacement that gets discarded. **It would not catch C-314 given infinite
time**, and a full run is ~10–20k mutants over 38 crates against a 6 h runner ceiling. Record this in
the design doc so it is not re-litigated. (It remains plausible for a *different* debt — untested
branches in the pure L0 crates — scoped and nightly. Not this story.)

## Acceptance

- [x] **Failing-first, of the strongest available kind:** the census, run at this story's base
      commit, reports exactly `crates/flux-cli/src/lab_cmd.rs:52` and `crates/flux-cli/src/review.rs:185`
      as unpinned. Paste the failure output. **A census that is green on its first run has not been
      demonstrated.**
- [x] A new alias-resolving `syn::visit::Visit` scanner in `crates/flux-codegate/src/lib.rs`
      (`pin_seams(src) -> Vec<Seam>`) finds method calls whose receiver chain roots at
      `flux_sdk::Client::builder` or `flux_sdk::FlowClient::builder`, skips `#[cfg(test)]` items, and
      records a **byte span** (not a line) so C-329's runner can excise a multi-line chain. Unit-test
      it against fixtures including a renamed import (`use flux_sdk::Client as C;`) and a
      `#[cfg(test)]` decoy — the shape `direct_io_scanner_resolves_imports_aliases_and_all_io_families`
      already uses.
- [x] Pins and exemptions are read by the **existing** `allow_reason` (`lib.rs:1748`) with new
      markers `flux-pin:` and `flux-pin-exempt:`. **A bare marker with no text is not a pin** —
      assert it, mirroring `direct_io_allowance_requires_a_real_reason_immediately_above_the_call`.
      Do not write a second waiver reader.
- [x] Every pinned test **exists**, resolved against `workspace_test_files` (`lib.rs:2773`) and the
      `#[cfg(test)]` modules the source walker sees. A pin naming a nonexistent test reds, with its
      own fixture test. This is the anti-drift half and it is free.
- [x] **Anti-vacuity**, in the idiom of `architecture_source_walk_covers_both_workspaces`
      (`lib.rs:2998`) and `catalog_coherence.rs:957`: assert a minimum files-scanned and seams-found
      count, and cap the number of `flux-pin-exempt` entries so exemptions cannot quietly become the
      norm.
- [x] **C-314 is closed by this story, not deferred.** Two *independently attributable* tests — one
      that reds when `review.rs:185`'s `.resource_limits(resource_limits)` is deleted, one for
      `lab_cmd.rs:52`. Prove each by making exactly that deletion and showing the test name in the
      failure output. **One test observing both is not acceptable** — that is the mistake C-305's
      first round made.
- [x] The census runs beside `cargo test -p flux-codegate` in CI (`.github/workflows/ci.yml:71`) and
      is named in `AGENTS.md`'s dev-loop block.
- [x] `docs/designs/unobserved-wiring.md` records the predicate and why it is narrow, the
      cargo-mutants rejection (operator set first, cost second), why no existing guard is subsumed,
      and the non-coverage list below.
- [x] Full gate green in both workspaces.

## Notes

- **No existing guard is subsumed, and two must not be.** The ~10 bespoke guards answer *"does this
  exist / is it classified?"*; this asks *"does a test observe it?"* — orthogonal. And
  `capability_widenings` (`crates/flux-plugin/src/host/refresh.rs:403`) and `pin_granted_authority`
  (`:343`) fail at **compile time**, which is strictly stronger than anything test-based. The design
  pressure runs the other way: prefer an exhaustive destructure whenever the invariant is "a field
  set is classified".
- Use the in-source marker, **not** the `const ALLOW` table shape. A pin keys on a call site, which
  has no stable identity but position, so a `(file, line)` const rots on every edit above it —
  `catalog_coherence` already avoids this by keying on `(module, seam, source)`. The marker travels
  with the line.
- ⚠ **Will not cover** — state this in the design doc rather than discovering it later:
  [C-313](C-313-url-encoder-consolidation-and-key-pinning.md) (an ordinary expression, not a builder
  seam), [C-324](C-324-pane-queue-overflow-is-a-silent-success.md) (a *missing* signal, not an
  unobserved one), wiring expressed as data rather than as a call site, and **the semantics of a
  pinned test** — a pin proves the test dies when the span goes, not that it dies for the right
  reason. This is a **coverage floor, not a proof**; the reviewer still reads the test.
- Follow-ons already scoped: C-329 (the runner + `scan.rs` extraction), C-330 (widen the predicate
  across `crates/*/src`), C-331 (compile-time destructure anchor for `Config`/`Limits`).

## Progress

**Half A is complete.** Base `0df177c2`; the census's first run reported exactly the two named sites
and nothing else — output in the implementation report and reproducible by reverting either pin
comment.

Landed:

- `flux_codegate::pin_seams` (`lib.rs:1930`) + `Seam` (`:1771`) with `span_start`/`span_end` byte
  offsets and a `Seam::span() -> Range<usize>` for C-329. `flux_codegate::test_function_names`
  (`:1951`) collects the resolution universe. `DirectIoAliasCollector` was renamed
  `ImportAliasCollector` (nothing in it was I/O-specific) and now also resolves `use flux_sdk::*`.
- Census `every_sdk_client_wiring_seam_pins_a_test_that_observes_it` (`:3478`); the per-seam decision
  is factored into `pin_verdict` (`:3419`) so the fixture test `pin_verdicts_distinguish_pinned_-`
  `drifted_and_exempt` (`:3436`) exercises the same code the workspace walk does.
- C-314 closed: `crates/flux-cli/src/lab_cmd.rs:78` and `crates/flux-cli/src/review.rs:160`, each
  pinned to its own test. Both call sites were extracted into named seams (`record_client_from`,
  `review_flow_client`) because neither was reachable from a test before — that unreachability *is*
  how C-314 happened.

Notes for whoever picks up C-329 or C-330:

- The predicate is one setter (`resource_limits`). `pinned_setter` (`lib.rs`) is the single place to
  widen it; the design doc records why widening now would produce twelve mostly-noise seams.
- `MAX_PIN_EXEMPTIONS` is 1 with 0 in use, so there is exactly one slot before the cap must be raised
  under review.
- The census is a **coverage floor**: it proves a pin resolves, not that the test dies. Deleting a
  pinned wiring line makes the seam (and so the census entry) disappear entirely — that gap is
  precisely C-329's job, and until it lands the two attributable tests are what hold the line.
