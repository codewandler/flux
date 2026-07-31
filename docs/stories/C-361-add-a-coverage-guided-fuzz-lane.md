---
id: C-361
title: Add a coverage-guided fuzz lane with a persistent corpus
pillar: Core
status: backlog
epic: assurance-lane-residuals
design: docs/designs/assurance-lane-residuals.md
note: "no fuzz/ dir, no cargo-fuzz, no arbitrary, no seed corpora — the 'adversarial corpus' is a seeded deterministic generator over committed fixtures and keeps nothing it finds"
---

# Add a coverage-guided fuzz lane with a persistent corpus

## Goal

Search for input shapes nobody thought to write down, and keep what the search finds.

## Acceptance

- [ ] Fuzz targets exist for the seams the deterministic corpus already covers by hand: the
      flux-lang lexer and parser, plugin NDJSON framing, URL normalisation, and pack extraction.
- [ ] A corpus is persisted between runs and committed or cached, so coverage accumulates.
- [ ] Every crash the lane finds becomes a committed regression case in the deterministic corpus —
      the two instruments feed each other rather than duplicating.
- [ ] The lane runs on a schedule with a bounded time budget and reports findings.
- [ ] The distinction between the deterministic corpus and the fuzzer is stated where both are
      documented.

## Progress

- 2026-08-01 — filed from the ASSURE-01 lane split.

## Notes

- Scope the first landing to one or two targets with a real corpus rather than five with none.
