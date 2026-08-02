---
id: C-494
title: The MSRV repair does not restore a vulnerable PDF parser
pillar: Core
status: in-progress
priority: 0
note: "the 1.87-compatible pdf-extract downgrade pulled lopdf 0.38, vulnerable to untrusted recursive PDF objects; make safe extraction opt-in and keep default fetches opaque"
---

# The MSRV repair does not restore a vulnerable PDF parser

## Goal
Restoring Rust 1.87 compatibility never makes web.fetch parse an untrusted PDF with a dependency
line affected by RUSTSEC-2026-0187.

## Acceptance
- [x] cargo audit identifies the vulnerable lopdf 0.38 path introduced by the v0.52.2 MSRV repair.
- [x] The default flux-web graph contains no vulnerable PDF parser and still builds on Rust 1.87.
- [x] A default-build test proves detected PDF bytes remain opaque rather than falling through to a
      lossy raw response, while the opt-in pdf feature extracts through the fixed parser line.
- [ ] Workspace tests, clippy, formatting, codegate and audit pass, and a corrective 0.52 patch is
      published by CI.

## Notes
- Default and pdf-feature tests, the Rust 1.87 workspace build, workspace tests, clippy (including
  the pdf feature), formatting, codegate, changelog mirror, and cargo audit are green.
- The fixed lopdf line uses language syntax stabilized in Rust 1.88. PDF extraction is therefore an
  explicit feature with a feature-specific 1.88 floor; the default web and HTTP surface keeps 1.87.
- Ignoring the advisory is not an option: web.fetch parses remote, attacker-controlled bytes.
