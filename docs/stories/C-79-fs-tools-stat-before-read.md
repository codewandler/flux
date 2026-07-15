---
id: C-79
title: Stat-before-read so fs tools can't OOM the host on a large file
pillar: Core
status: done
priority: 6
epic: harness-hardening
design: docs/designs/harness-hardening.md
note: "DoS (High) — read/grep/file_stat slurp whole file into RAM before the size cap; named pipe hangs"
---

# Stat-before-read so fs tools can't OOM the host on a large file

## Goal
Stop the fs tools from loading an entire file before checking its size. `read` does an uncapped
`read_file_bytes` and only compares against `READ_BYTE_CAP` afterward (not approval-gated), so a large
in-workspace file loads entirely (~2× peak after decode) and can OOM-kill the host; a workspace named
pipe hangs it (no read timeout). `System::file_size()` exists for exactly this but is unused.

## Acceptance
- [ ] Failing-first test: `read`/`grep`/`file_stat` on an over-cap file returns the over-cap guidance
      without materializing the whole file (assert peak/bounded read).
- [ ] Stat-first (or `file.take(READ_BYTE_CAP + 1)`) on every slurp path; a read timeout / non-regular-file
      guard so a named pipe cannot hang the tool.

## Progress
- **2026-07-15 — DONE for `read` (unit-test + clippy verified; full gate pending).** `ReadTool` now
  stats first (`file_size`): an unbounded read of an over-cap file returns paging guidance *without*
  materializing it. The actual read goes through a new `System::read_file_bytes_capped(path, max)`
  that bounds memory at `MAX_READ_FILE_BYTES` (16 MiB) and **rejects non-regular files** (a FIFO/device
  can no longer hang the tool). Failing-first test `read_over_cap_file_returns_guidance_without_slurping`.
- Residual (follow-up): `grep` (`grep_file` at the `read_file_bytes` slurp) and the multi-file
  `read_section` path still slurp before their own caps — migrate them to `read_file_bytes_capped` too.

## Notes
- `crates/flux-tools/src/lib.rs:544` (`read`), `:1442`, `:1558`; `crates/flux-tools/src/extra.rs:81`;
  `read_file_bytes` at `crates/flux-system/src/lib.rs:1154`; unused `System::file_size()`.
- Design: [harness-hardening](../designs/harness-hardening.md).
