---
id: C-450
title: "Mechanically pin direct dependencies and gate new ones — the one place Pi's CI is ahead"
pillar: Core
status: ready
priority: 7
design: docs/designs/pi-comparison-remediation.md
epic: pi-comparison-remediation
areas: [ci, docs]
note: "the review credits flux with the DENSER assurance posture overall (9.0 vs 8.0) — this is the narrow slice where Pi is ahead: direct deps mechanically pinned, and new dependency lifecycle scripts requiring explicit review. Different ecosystem, transferable idea"
---

# The narrow slice where Pi's supply chain is tighter

## Goal

Make adding or moving a direct dependency a reviewed event rather than a diff nobody reads.

## The finding

The review credits Pi with:

> *"direct dependencies are mechanically pinned, and new dependency lifecycle scripts must be explicitly
> reviewed"* — plus CI that *"installs with lifecycle scripts disabled"* and a scheduled
> *"vulnerability and npm registry-signature audit."*

⚠ flux is **ahead overall** on this axis — 9.0 vs 8.0, credited with *"architecture gates, no-backend
tests, CodeQL, targeted Miri and artifact attestations"*, and Actions already commit-pinned with a CI
guard. This story is only the slice where Pi is tighter, and it transfers even though the ecosystem
differs: npm lifecycle scripts have no cargo equivalent, but **build scripts do**, and "a new direct
dependency is a decision" is ecosystem-independent.

## Acceptance

- [ ] A mechanical check that a **new or moved direct dependency** fails CI until acknowledged. The
      repo already has the pattern — `scripts/check-feature-gated-tests.sh` fails for any (package,
      feature) pair with no declared disposition, which is exactly this shape for a different axis.
- [ ] ⚠ **`build.rs` is the cargo analogue of a lifecycle script** — arbitrary code at build time. A new
      dependency that ships one should be visible in review rather than discovered later.
- [ ] The check names *what changed and what to do*, so it is a prompt rather than an obstacle. A gate
      people route around teaches nothing.
- [ ] ⚠ Do not duplicate what already exists: `scripts/check-crate-versions.sh`, the Actions-pinning
      guard and the security-audit workflow are already here. Extend rather than adding a fourth
      supply-chain script.
- [ ] Full gate green.

## Notes

- ⚠ Known blind spot, worth fixing in the same pass if cheap: `check-crate-versions.sh` is structurally
  blind to a breaking change in a workspace-versioned published crate — C-396 hit exactly that, where a
  `!` in a commit title was the only machine-readable signal.
- Registry-signature verification is the other half of Pi's audit; crates.io's story differs, so decide
  whether there is an equivalent worth having or record that there is not.

## Progress
- Filed 2026-08-02 from the Pi comparison.
