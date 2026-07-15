---
id: C-78
title: Jail sqlite_query to the workspace (or raise risk for out-of-jail paths)
pillar: Core
status: done
priority: 3
epic: harness-hardening
design: docs/designs/harness-hardening.md
note: "SECURITY (High, verified) — reads any on-disk DB (cookies, token stores) at Risk::Low, no approval"
---

# Jail sqlite_query to the workspace (or raise risk for out-of-jail paths)

## Goal
Stop `sqlite_query` from being a read-exfiltration primitive: it `~`-expands and opens *any* absolute
path read-only at `Risk::Low` (no approval prompt), so a model can read `~/.mozilla/.../cookies.sqlite`,
credential stores, or any user-owned `.db` straight into the tool result — defeating the fs-jail for
SQLite files.

## Acceptance
- [ ] Failing-first test: `sqlite_query` against an absolute path outside the workspace/read-roots is
      refused (or requires approval), while an in-workspace `.db` still works.
- [ ] Relative/`~` paths route through `Workspace::resolve` like the other fs tools.
- [ ] Out-of-workspace absolute paths require an allowlist entry or are raised above `Risk::Low` so the
      approval gate fires.

## Progress
- **2026-07-15 — DONE (unit-test + clippy verified; full gate pending).** `sqlite_query` now jails the
  `db` path via `jail_sqlite_path`: relative paths go through `Workspace::resolve_read`; an
  out-of-workspace absolute/`~` path is allowed only when it canonicalizes under `~/.flux` (the
  advertised session-DB use), else refused before opening. Risk stays `Low` (the jail, not a prompt,
  closes the vector). Failing-first test `sqlite_query_refuses_database_outside_the_jail` passes; 133
  flux-tools tests green.

## Notes
- `crates/flux-tools/src/extra.rs:234` (`risk: Risk::Low`), `:264` (`execute`, arbitrary open), `:241`
  (`permission_subjects`). The tool description currently advertises out-of-workspace absolute paths.
- Design: [harness-hardening](../designs/harness-hardening.md).
