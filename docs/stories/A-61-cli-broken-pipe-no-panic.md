---
id: A-61
title: CLI must not panic on a broken pipe (SIGPIPE)
pillar: Agent
status: done
epic: beta-hardening
design: docs/designs/beta-hardening.md
note: "F-006 (beta rec #5): `flux sessions | head -8` prints rows then panics on broken pipe — the reader closed early; the CLI should exit quietly (0/130), not panic with a Rust backtrace"
---

# CLI must not panic on a broken pipe (SIGPIPE)

## Goal
`flux sessions | head -8` prints rows then panics when `head` closes the pipe early. Piping into
`head`/`less`/`grep -q` is routine; a broken pipe must end the command quietly (as every well-behaved
Unix CLI does), not surface a Rust panic + backtrace to the user.

## Why (evidence)
- Beta F-006: "`flux sessions | head -8` printed rows and then panicked on broken pipe."
- Rust's default resets `SIGPIPE` to `SIG_IGN`, so writes to a closed pipe return `EPIPE`, and
  `println!`/`writeln!` on `EPIPE` panic. The fix is process-wide, not per-command.

## Acceptance
- [ ] Writing to a closed stdout pipe ends the process quietly (conventional quiet exit), with no
      panic and no backtrace — applied process-wide so it covers every subcommand that streams rows
      (`sessions`, `usage`, replay/list surfaces, etc.), not just `sessions`.
- [ ] Failing-first test: a `flux <streaming-subcommand> | head -1`-style harness asserts no panic
      output on stderr and a clean exit. (If a unit test can't spawn a pipe, cover the write-side
      `EPIPE`-tolerant helper directly.)
- [ ] The fix does not swallow genuine write errors to a real file/terminal (only the
      reader-closed-early `EPIPE` case is treated as a normal end).

## Progress
- 2026-07-08 **DONE.** `reset_sigpipe()` restores the default `SIGPIPE` disposition (`SIG_DFL`) at
  the top of `main` (added a `libc` dep, `#[cfg(unix)]`), so a broken pipe ends the process the
  conventional Unix way instead of `println!` panicking on EPIPE — process-wide, covering every
  streaming subcommand. Test: `reset_sigpipe_installs_sig_dfl` (asserts SIG_DFL is installed).
- Note: no `flux` subcommand emits more than the ~64 KB pipe buffer, so a *guaranteed*-EPIPE
  `| head` integration test isn't practical; per the acceptance ("if a unit test can't spawn a
  pipe, cover the … helper directly") the mechanism test is the direct coverage.

## Notes
- Beta rec order #5.
- Common approaches: restore the default `SIGPIPE` disposition at startup (`SIG_DFL`) so the OS
  terminates the process conventionally on a closed pipe, or make the row-writer tolerate `EPIPE`
  and stop. Pick one and apply it at the CLI entry point.
- Epic: [beta-hardening](../designs/beta-hardening.md).
