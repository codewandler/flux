---
id: C-57
title: Route flux_reload process replacement through the guarded process path
pillar: Core
status: backlog
note: "2026 codebase review: dev reload directly execs/spawns std::process::Command outside flux-system's single guarded process seam."
---

# Route flux_reload process replacement through the guarded process path

## Goal

Close the direct process-launch seam in the dev-only `flux_reload` tool so every OS process start or replacement remains auditable through the same guarded `flux_system::System` boundary.

## Acceptance

- [ ] `ReloadTool::execute` no longer calls `std::process::Command` directly to exec/spawn the replacement process.
- [ ] The chosen behavior is explicit: either a narrow guarded re-exec/replacement primitive lives in `flux-system`, or `flux_reload` rebuilds only and returns manual restart instructions.
- [ ] A regression test or codegate/source-scan prevents non-test tool/runtime/plugin paths from adding new direct `std::process::Command` launches outside `flux-system` without an explicit exception.
- [ ] Documentation/changelog entries are updated if user-visible reload behavior changes.
- [ ] Relevant gates pass, including the new regression test and `cargo test -p flux-codegate`.

## Progress

- 2026-07-14 — filed from repository review finding. The build step may stay as-is if it already routes through guarded system APIs; the post-build process replacement is the unsafe seam to remove.

## Notes

- Governing invariant: `AGENTS.md` says all process creation must go through `flux_system::System` and not add another `Command::new` seam.
- Current direct seam: `crates/flux-tools/src/lib.rs` `ReloadTool::execute` uses `std::process::Command::new(&exe).args(&args[1..]).exec()` on Unix and `.status()` on non-Unix after a successful rebuild.
- Existing guarded command construction lives in `crates/flux-system/src/lib.rs`; any re-exec primitive should reuse its env-clearing, workspace-pinning, argv-only, and audit semantics rather than duplicating them.
