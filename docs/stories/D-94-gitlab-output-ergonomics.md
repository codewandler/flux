---
id: D-94
title: gitlab — output ergonomics (decoded text) & quiet pure reads
pillar: Agent
status: backlog
priority:
epic: gitlab-plugin-hardening
design: docs/designs/gitlab-plugin-hardening.md
note: "file.show returns base64 only (agents want text); plain read/list calls contribute datasource records and print stderr noise, surprising scripts expecting pure reads (GL-006/015)"
---

# gitlab — output ergonomics (decoded text) & quiet pure reads

## Goal
Make the common read paths pleasant and side-effect-free: an optional decoded-text field for UTF-8
files, and no hidden datasource contribution or stderr noise on a plain read.

## Why (evidence)
A beta pass found `repository.file.show` returns GitLab-API base64 content only — faithful but
awkward for agents and CLI users who usually want text — and that plain read/list calls contribute
records to the local datasource and print a `(N record(s) contributed)` line on stderr, which
surprises scripts and users expecting a pure read.

## Acceptance
- [ ] `repository.file.show` adds an optional `decoded_content`/`text` field when the file is UTF-8
      text and within `max_bytes` (raw `content`/`encoding` kept); large/binary files are unaffected
      (GL-006).
- [ ] Direct read/list calls' datasource contribution is opt-in (or at minimum the stderr
      contribution line is suppressed by default / gated behind a verbose flag), so a pure read has
      no visible side effects (GL-015).
- [ ] `cargo build/test/clippy -D warnings/fmt` green for `gitlab`; MockHost tests per changed op.

## Progress
- Not started.

## Notes
- The datasource-contribution-on-read behavior may be a host-level convention rather than gitlab-
  specific; confirm where the contribution is triggered before choosing gitlab-local vs host-level
  fix.
