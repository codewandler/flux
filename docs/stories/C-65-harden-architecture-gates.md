---
id: C-65
title: Make architecture gates resolve real dependencies and process APIs
pillar: Core
status: done
epic: architecture-review-2026-07-14
design: docs/designs/architecture-review-2026-07-14/review.md
note: codegate misses renamed/target/build dependencies plus aliased and Tokio process construction
---

# Make architecture gates resolve real dependencies and process APIs

## Goal

Turn the layering and one-process-seam invariants into compiler/package-aware gates that cannot be
evaded by Cargo aliases, target tables, imports, type aliases, or a different process API.

## Acceptance

- [x] The layer gate uses `cargo_metadata` package identities and checks every documented non-dev
      dependency kind, including renamed dependencies plus target-specific and build dependencies.
- [x] Fixture tests prove an inner crate cannot hide an outer dependency behind `package =`, a target
      table, or a build-dependency; dev-dependency policy remains explicit.
- [x] Forbidden process construction is detected through imports/aliases, multiline syntax,
      `std::process::Command`, and `tokio::process::Command`, preferably via compiler/clippy or a Rust
      syntax tree rather than substring scanning.
- [x] Only the canonical `flux-system` command builder is allowed; a second raw seam inside
      `flux-system` itself fails unless represented by a narrow reviewed allow entry.
- [x] The gate scans root crates and the nested plugin workspace and has self-tests for every prior
      false-negative shape.
- [x] After C-61, the same resolver-aware mechanism prevents new raw project-path IO outside the
      documented trusted control-plane owners.

## Progress

- 2026-07-14 — Replaced manifest-text dependency checks with `cargo_metadata` package identities and
  syntax-tree process/project-IO scanners across both workspaces. Alias, target/build dependency,
  Tokio/std command, single-use seam, and project-metadata fixtures exercise prior blind spots.

## Notes

- Review: [architecture review](../designs/architecture-review-2026-07-14/review.md).
- The scanner added during C-57 improves coverage but explicitly treats Tokio process creation as
  acceptable and still misses common alias shapes; this story closes the structural contract.
