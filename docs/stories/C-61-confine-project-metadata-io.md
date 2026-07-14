---
id: C-61
title: Confine project metadata IO to the guarded workspace
pillar: Core
status: done
epic: architecture-review-2026-07-14
design: docs/designs/architecture-review-2026-07-14/review.md
note: release blocker — repository-controlled context/config/skill paths can follow symlinks outside the workspace
---

# Confine project metadata IO to the guarded workspace

## Goal

Ensure repository-sourced context, roles, skills, and configuration cannot read or write outside the
guarded workspace, while keeping user-global control-plane configuration explicit and separate.

## Acceptance

- [x] Failing-first tests prove symlinked `AGENTS.md`, `CLAUDE.md`, `.flux/context.md`, project skills,
      and project roles cannot load content from outside the workspace or expose it to a provider.
- [x] Failing-first tests prove `.flux/config.toml` persistence cannot overwrite an out-of-workspace
      target through a file or parent-directory symlink.
- [x] All project-local metadata IO resolves through `Workspace/System` path identity and guarded
      read/write APIs; missing optional files remain harmless, but guard failures are not silently
      converted into absent content.
- [x] Pure config, skill, and role parsing is separated from filesystem discovery/loading so L0
      contracts take injected bytes/metadata rather than owning concrete project IO.
- [x] User-global config/credential/skill roots are documented as trusted control-plane IO with their
      own explicit boundary; existing precedence and opt-in skill activation remain unchanged.
- [x] Project config writes are atomic and preserve unrelated settings; regression coverage includes
      Unix symlinks and the platform-appropriate equivalent or an explicit unsupported disposition.
- [x] `flux-codegate` gains a maintainable structural guard against new raw project-path IO after
      C-65 provides the resolver-aware mechanism.

## Progress

- 2026-07-14 — Centralized project metadata discovery in `flux_runtime::metadata` over confined
  `System` instances and atomic guarded writes. Context, skill, role, config, symlink, and
  `no_raw_project_metadata_io_outside_guarded_boundary` tests pin the boundary.

## Notes

- Review: [architecture review](../designs/architecture-review-2026-07-14/review.md).
- Coordinate role discovery errors with A-85 and the structural gate with C-65.
- Primary evidence: `flux-runtime/src/context.rs`, `flux-skill/src/lib.rs`,
  `flux-agent/src/role.rs`, and `flux-config/src/lib.rs`.
