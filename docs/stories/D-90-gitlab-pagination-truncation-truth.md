---
id: D-90
title: gitlab — pagination & truncation truth (caps, flags, byte-safe previews)
pillar: Agent
status: done
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
- [x] `file.show max_bytes` truncates on a *decoded-byte* boundary (or documents that it caps decoded
      bytes) so `content` is always valid for its `encoding` (GL-013). → base64 content decodes,
      caps decoded bytes, re-encodes; plain text keeps the char-boundary byte cap.
- [x] `max_*_bytes` caps are inclusive of any truncation marker — the returned string never exceeds
      the requested maximum (GL-035). → `cap_bytes` + the search.blobs data cap budget the marker;
      a cap too small for the marker returns the bare prefix (the `*_truncated` flag still signals).
- [x] `compare` sets a single top-level truncation signal when any file diff is truncated, or
      documents that consumers must scan per-file `diff_truncated` (GL-014). → `truncated` is the
      aggregate (files dropped ∨ any diff cut ∨ commits cut), with per-cause flags alongside.
- [x] List ops expose a "results were capped" signal (and/or a page/cursor input) so a capped first
      page is distinguishable from a complete set (GL-019). → explicit 1-based `page` input on all
      15 paginating ops + over-cap limits now REJECT via schema maxima instead of silently clamping.
- [x] `mr.changes` applies the `file` filter *before* the page cap, or paginates until the target
      file is found, so asking for a specific file cannot return zero (GL-042).
- [x] `mr.diff.lines` and `mr.discussion.create` resolve files across all changed files (paginate)
      rather than a hard-coded first 200 (GL-043). → shared `fetch_file_diff` pagination helper.
- [x] `mr.changes` gains a top-level file-count truncation flag distinct from per-file
      `diff_truncated` (GL-044).
- [x] `compare` gains a `max_commits` cap and a commit-truncation marker (GL-045). → default 50,
      max 500; `commit_count` keeps the full total; `commits_truncated` flags the cut.
- [x] `mr.diff.lines` can address a deleted line via `old_line` (not only `new_line`) (GL-047).
- [x] `repository.archive` gains a size cap and/or a dry-run byte estimate so a "low-risk read"
      cannot download an unbounded archive (GL-023). → refuses results over `max_bytes`
      (default 50 MiB) with an actionable error.
- [x] `cargo build/test/clippy -D warnings/fmt` green for `gitlab`; MockHost tests per changed op.

## Progress
- [x] 2026-07-10: implemented — decoded-boundary file caps, marker-inclusive byte caps, compare
      commit cap + aggregate truncation flag, paginated `mr.changes`/`fetch_file_diff` with
      filter-before-cap, `old_line` anchoring, archive size cap, `page` inputs + schema-max limit
      rejection across the 15 paginating ops. 7 new tests + 2 re-pinned cap tests (74 green);
      plugins workspace gate green; schema surface verified end-to-end via `--dry-run` (over-cap
      limit rejected, `page`/`max_commits`/`old_line` accepted clean).

## Notes
- GL-023 (archive size) sits at the seam with D-88 (validation) and D-92 (scope estimate); kept here
  with the other output-size caps.
