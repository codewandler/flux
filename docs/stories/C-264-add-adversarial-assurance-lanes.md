---
id: C-264
title: "Add adversarial parser, memory-safety, and static-analysis CI lanes"
pillar: Core
status: done
priority: 9
epic: adversarial-review-remediation-2026-07-30
design: docs/designs/adversarial-review-remediation-2026-07-30.md
areas: [ci, providers, flux-lang, flux-plugin]
note: "LOW/MEDIUM assurance — extensive author-written tests have no fuzz, Miri/sanitizer, or SAST complement"
---

# Add adversarial parser, memory-safety, and static-analysis CI lanes

## Goal

Continuously exercise the input classes most likely to violate the envelope—provider frames,
Flux-Lang parsing, plugin framing, URL normalization, and archive extraction—with independently
generated inputs and static/memory-safety analysis.

## Acceptance

- [x] Add bounded, deterministic fuzz/property targets for at least provider stream envelopes,
      Flux-Lang parsing, plugin framed NDJSON, URL/redirect normalization, and pack extraction.
- [x] Seed each target with the existing regression corpus and pin known-bad fixtures that the live
      production oracle must reject before generated mutations run.
- [x] CI has a time-bounded scheduled fuzz lane plus a PR-smoke lane that is reliable offline after
      dependency fetch.
- [x] Add a pinned SAST workflow and a bounded Miri or sanitizer lane over compatible high-risk pure
      crates; unsupported targets are explicitly enumerated, not silently skipped.
- [x] Failures preserve a secret-free log plus the deterministic seed/case/recipe needed to promote
      the smallest failing generated mutation into a regression test.
- [x] Action pins, workflow policy, targeted tests, and the standard gate are green.

## Progress

- Added a separate `adversarial-assurance` workflow with SHA-pinned Rust CodeQL, a bounded PR corpus
  smoke, a larger weekly corpus run, and a date-pinned nightly Miri lane. Provider/Tokio, DNS/socket,
  and compression/filesystem exclusions from Miri are explicit and remain covered by corpus jobs.
- Reused the exhaustive provider envelope corruption corpus and the 1,000-seed Flux-Lang AST
  round-trip property. Added deterministic generated-input targets over the production plugin NDJSON
  decoder, URL/redirect normalization plus DNS answers, and plugin zip/tar.xz extraction.
- Moved plugin frame decoding into the pure protocol crate so production, corpus, and Miri exercise
  one implementation rather than a test-only approximation.
- Added committed known-bad seeds and a runner self-test that demonstrates missing/renamed test
  selectors and an incomplete CodeQL workflow are rejected. Cargo selectors are enumerated before
  execution so a filter matching zero tests cannot pass, including the two Miri selectors.
- A closure review replaced the workflow's raw substring policy with parsed YAML assertions over
  enabled job conditions, CodeQL permissions/init/build/analyze ordering, corpus selectors, pinned
  artifact preservation, Miri targets, and a machine-readable exclusion inventory. Self-tests prove
  disabled-job and comment-only action decoys are rejected.
- Every run writes only deterministic seed/recipe coordinates and cargo output under
  `target/adversarial-artifacts`; pinned artifact upload preserves them on failure for regression
  promotion without capturing environment values or live provider data. Generated cases are
  single mutations of a committed seed (or one archive truncate/append recipe), so the reported
  coordinate is already the smallest generated operation rather than a falsely claimed shrinker.
- Local smoke and deep corpus runs, full plugin/protocol tests, scoped Clippy, YAML parsing, shell
  syntax, action-pin policy/self-test, diff checks, and the integrated workspace gate pass. The
  date-pinned Miri and hosted CodeQL jobs require GitHub runners; their local policy and selector
  preflights are green.

## Notes

- Evidence: reviews A and B assurance findings. These lanes supplement, not replace, independent
  review and the existing dependency-advisory workflow.
