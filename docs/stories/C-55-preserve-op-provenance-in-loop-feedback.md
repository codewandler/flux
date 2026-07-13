---
id: C-55
title: Preserve operation provenance in loop feedback
pillar: Core
status: done
note: "Ad-hoc E2E: low/medium gpt-5.5 computed grounded answers correctly but invented plan filenames because final feedback rendered four results as anonymous [read] blocks."
---

# Preserve operation provenance in loop feedback

## Goal

Let later planner rounds cite and reason about gathered evidence reliably. A transcript result must
retain a concise, safe source label such as the path passed to `read` or the query/scope passed to
`grep`, rather than reducing every result to an anonymous operation name.

## Acceptance

- [x] Bound, unbound, memo, and pipe transcript entries use the existing bounded read/grep summary
      prefix derived from resolved arguments.
- [x] Canonical values, model-facing result bodies, sink events, and authorization are unchanged.
- [x] No generic argument dump is added; only the existing read/grep safe summary seam is reused.
- [x] A runtime regression proves multiple read results retain distinct paths in the transcript.
- [x] Re-running the same `/tmp` question produces correct file citations in final feedback/output.
