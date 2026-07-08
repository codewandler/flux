---
id: D-90
title: gitlab — pagination & truncation truth (caps, flags, byte-safe previews)
pillar: Agent
status: backlog
priority:
epic: gitlab-plugin-hardening
design: docs/designs/gitlab-plugin-hardening.md
note: "diff/byte/commit caps and truncation flags are dishonest: base64 file truncation yields invalid fragments, compare.truncated stays false while a file diff is cut, list clamping is silent, mr diff resolution is a hard 200-file cap, compare commits are uncapped (GL-013/014/019/023/035/042/043/044/045/047); extends D-38"
---

# gitlab — pagination & truncation truth

## Goal
Make every gitlab bound-output operation tell the truth about what it returned: byte caps that stay
within the cap, file/commit caps with an honest top-level truncation flag, and diff resolution that
does not silently miss files beyond a hidden page limit. Extends the completed [D-38](D-38-gitlab-parity-ports.md)
byte-cap/pagination ports rather than duplicating them.

## Why (evidence)
A beta pass found the caps are approximate and the flags incomplete: `file.show max_bytes` truncates
the *base64 string* (yielding an invalid fragment), `max_*_bytes` caps are exceeded by the appended
truncation marker, `compare.truncated` reports false while a per-file diff is truncated, list ops
silently clamp high limits with no "capped" signal, `mr.changes`/`mr.diff.lines`/`discussion.create`
resolve files against a hard-coded first-200-file page, and `compare` returns all commits uncapped.

## Acceptance
- [ ] `file.show max_bytes` truncates on a *decoded-byte* boundary (or documents that it caps decoded
      bytes) so `content` is always valid for its `encoding` (GL-013).
- [ ] `max_*_bytes` caps are inclusive of any truncation marker — the returned string never exceeds
      the requested maximum (GL-035).
- [ ] `compare` sets a single top-level truncation signal when any file diff is truncated, or
      documents that consumers must scan per-file `diff_truncated` (GL-014).
- [ ] List ops expose a "results were capped" signal (and/or a page/cursor input) so a capped first
      page is distinguishable from a complete set (GL-019).
- [ ] `mr.changes` applies the `file` filter *before* the page cap, or paginates until the target
      file is found, so asking for a specific file cannot return zero (GL-042).
- [ ] `mr.diff.lines` and `mr.discussion.create` resolve files across all changed files (paginate)
      rather than a hard-coded first 200 (GL-043).
- [ ] `mr.changes` gains a top-level file-count truncation flag distinct from per-file
      `diff_truncated` (GL-044).
- [ ] `compare` gains a `max_commits` cap and a commit-truncation marker (GL-045).
- [ ] `mr.diff.lines` can address a deleted line via `old_line` (not only `new_line`) (GL-047).
- [ ] `repository.archive` gains a size cap and/or a dry-run byte estimate so a "low-risk read"
      cannot download an unbounded archive (GL-023).
- [ ] `cargo build/test/clippy -D warnings/fmt` green for `gitlab`; MockHost tests per changed op.

## Progress
- Not started.

## Notes
- GL-023 (archive size) sits at the seam with D-88 (validation) and D-92 (scope estimate); kept here
  with the other output-size caps.
