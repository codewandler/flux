---
id: C-32
title: "read() on a directory returns recoverable guidance, not a plan-halting raw io error"
pillar: Core
status: done
priority: 3
epic: flux-lang-evolution
note: "s_362: six `read(\"crates/flux-policy/src\")`-style calls halted the orient plan with `io error: Is a directory (os error 21)` — a weak-model guardrail: return a ToolResult::error with 'is a directory — glob it first' guidance the planner can react to in-turn"
---

# read() on a directory returns recoverable guidance

## Goal
`ReadTool::execute` (flux-tools/src/lib.rs:520) propagates the raw
`Is a directory (os error 21)` io error via `?`, halting the plan node. Weak models routinely
`read()` a directory (s_362 did it six times in one orient plan). Catch the directory case (test
`is_dir` on the path — don't string-match the io error) and return `ToolResult::error` with
actionable guidance ("`<path>` is a directory — list it with glob(\"<path>/**/*.rs\") first, then
read specific files"), which feeds back into the loop as a repairable failure instead of a halt.
Apply the same guidance in the windowed/`read_section` path.

## Acceptance
- [x] Failing-first test: `read` on a directory returns an is_error ToolResult whose content names
      the path and suggests glob — the flow continues (today: plan halt on the io error).
- [x] Both the whole-file and windowed read paths covered.
- [x] A genuinely missing file still errors as today.

## Progress
- 2026-07-03 filed from s_362 forensics.
- 2026-07-03 implemented: added `System::is_dir` (flux-system/src/lib.rs) — resolves the path
  through the same `resolve_read` jail as `file_mtime`/`read_file_bytes`, then checks
  `tokio::fs::metadata(..).is_dir()`; a workspace-escaping path still errors loudly, a
  missing/non-directory path collapses to `Ok(false)` (the caller's own read stays the source of
  truth for "missing"). `ReadTool::execute`'s single-file branch (covers both the unbounded
  whole-file read and the windowed offset/limit read — they share the same `read_file_bytes` call)
  now checks `ctx.system.is_dir(path).await?` first and returns
  `ToolResult::error(directory_read_guidance(path))` instead of propagating the raw io error. The
  `read_section` helper (the `read_many`/multi-path-glob machinery) got the same check, scoped to its
  own section so one directory among several paths doesn't halt the others. New shared helper
  `directory_read_guidance(path)` in flux-tools/src/lib.rs. Failing-first test
  `read_on_a_directory_returns_repairable_guidance_not_a_raw_io_error` (flux-tools/src/lib.rs) covers
  whole-file, windowed, and multi-path reads on a directory plus a still-erroring missing file —
  failed before the fix with `Io(Os { code: 21, kind: IsADirectory, message: "Is a directory" })`
  propagating through `.unwrap()`, green after. Added a mirroring unit test
  `is_dir_distinguishes_directories_files_and_missing_paths` in flux-system/src/lib.rs (directory,
  file, missing path, workspace-escape). Gate green: `cargo test -p flux-lang -p flux-tools` (also
  ran flux-system's own suite), `cargo clippy -p flux-lang -p flux-tools --all-targets -- -D
  warnings` clean, `cargo fmt -p flux-lang -p flux-tools --check` clean, `cargo test --workspace`
  green.
