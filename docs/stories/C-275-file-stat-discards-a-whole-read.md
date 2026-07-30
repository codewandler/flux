---
id: C-275
title: "`file_stat` reads the whole target a second time and throws it away"
pillar: Core
status: in-progress
priority: 8
epic: security-assurance
design: docs/designs/security-assurance.md
note: "envelope-integrity finding 4 from the 2026-07-29 review — the ONLY one of the four never closed, and it survived by never being filed rather than by decision. LOW, not a security defect"
---

# `file_stat` reads the whole target a second time and throws it away

## Goal

Close the last open finding of the 2026-07-29 envelope-integrity review. It is the only one of that
review's four that was never addressed, and — the part worth recording — **it survived by never being
filed, not by anyone deciding to defer it.** C-192, C-193 and C-194 map to findings 1–3; finding 4 fell
off the edge with no story, no "won't do" and no reason. C-267's closure pass found the gap while
mapping evidence, and deliberately handed it over rather than repairing it.

## Acceptance

- [x] `file_stat` no longer reads the file's entire contents solely to discard them. Verified at
      `crates/flux-tools/src/extra.rs:96-107`: `read_file_bytes(path)` is awaited and the result passed
      to `.map(|_| "(mode unavailable)".to_string())` — the bytes are never used.
- [x] The dead `mode_str` binding and its `let _ = mode_str;` line go with it (`:107`). Its trailing
      comment promises "we surface it as a note below" and **no such note exists** — verified: the
      emitted JSON is `{path, size_bytes, line_count, mtime_unix}` and the view has four lines, neither
      mentioning mode (`:108-118`).
- [x] Either the op reports a mode honestly or it says nothing about mode at all. A comment claiming a
      note that is not emitted is the actual defect here — worse than an absent field, because it tells
      the next reader the case is handled.
- [x] **The author's instinct is preserved, and this is the constraint that matters.** The comment at
      `:102` records why `std::fs::metadata` was declined: on the raw caller-supplied string it would
      escape the guarded jail. Any fix must go through the guarded surface — and note that a
      `host_path_identity`-style physical reduction now exists on `System`, which did not when this was
      written. Do **not** reach for `std::fs`; `scripts/check-no-direct-io.sh` will refuse it anyway.
- [x] A test pins whichever contract is chosen, so a future edit cannot silently reintroduce a
      read-and-discard. Name it in this story.
- [x] Full gate green, and the finding is marked closed in
      [`reviews/2026-07-30-security-assurance-closure.md`](../../reviews/2026-07-30-security-assurance-closure.md)
      so the next reviewer sees it closed rather than re-deriving it — which is the whole point of that
      artifact.

## Progress

- **Done on `impl/C-275`.** All six acceptance items satisfied; full gate green in an isolated
  worktree with its own `target/`.
- **The redundant read is gone.** `FileStatTool::execute` now calls `read_file_bytes` exactly once
  (for `line_count`); the `mode_str` binding, the `let _ = mode_str;` line and the comment promising
  "a note below" went with it (`crates/flux-tools/src/extra.rs:94-99`).
- **Mode: the op reports none, and now says so nowhere** — the choice under acceptance item 3.
  Reporting one *honestly* needs a guarded mode accessor on `System`; none exists, and the only
  other route is `std::fs::metadata` on the caller's raw string, which is exactly what the original
  author declined and what `check-no-direct-io.sh` refuses. `host_path_identity` reduces a path but
  yields no mode, and `std::fs` on its output would still be direct IO in a model-facing operation
  crate. Adding the accessor would mean editing `crates/flux-system/**`, fenced to C-276. So:
  silence, consistently — including the **spec description**, which advertised "octal mode" to the
  model that no emitted field ever carried. That was the same defect as the comment, one layer up.
- **Emitted contract is unchanged**: `{path, size_bytes, line_count, mtime_unix}`, four view lines.
  This is a pure cost-and-honesty fix, not a behaviour change.
- **Tests (acceptance item 5), both in `crates/flux-tools/src/extra.rs`, both red at the merge base:**
  - `file_stat_reads_the_target_exactly_once` — a source scan of the `FileStatTool` declaration.
    Behaviour cannot witness a discarded read (the JSON is byte-identical either way), so the
    contract is held at the source: it counts every whole-content guarded read
    (`read_file_bytes`, `read_file_bytes_capped`, `read_file`, `read_file_scoped`,
    `read_optional_text`) in the declaration and requires exactly one. It goes red if anyone adds a
    second guarded read for any reason, and **panics rather than passing vacuously** if it loses its
    anchor or the section stops looking like `file_stat`.
  - `file_stat_reports_no_mode_anywhere_in_its_contract` — spec description, emitted JSON keys, and
    the rendered view all checked for a mode claim; also pins the four keys and their values.
- **Review artifact updated** (acceptance item 6): finding 4 in
  `reviews/2026-07-30-security-assurance-closure.md` is marked **CLOSED (C-275)** in its heading,
  its `top_findings` frontmatter entry and the Verdict bullet. The original analysis is kept verbatim
  beneath a dated closure note — it is the evidence, and the *process* lesson (the finding survived
  by never being filed) is the part that outlives the fix.
- ⚠ **C-186 is deliberately untouched.** This closes the epic's last envelope-integrity finding but
  not the epic: C-205 remains `blocked` on the `ratatui` 0.29 hold.

## Notes

- **Severity is LOW and this is not a security defect.** The guarded read is correct and confinement is
  intact; the cost is a wasted full read of an arbitrarily large file on every `file_stat`, plus a
  comment that misleads. Filed because an unfiled finding is invisible, not because it is urgent.
- Performance angle worth stating: `file_stat` is a cheap-sounding op that currently pays
  `read_file_bytes` on the whole target. On a large file that is a surprising cost for something a
  caller reasonably expects to be metadata-only.
- Provenance: the 2026-07-29 envelope-integrity review, finding 4. Found still-open by
  [C-267](C-267-security-assurance-closure-evidence.md), which recorded it as the epic's one
  unaddressed finding and is why [C-186](C-186-security-assurance-epic.md) is `in-progress` rather than
  `done`.
- ⚠ Closing this does **not** close C-186 by itself: that epic also waits on C-205 (`blocked` on the
  `ratatui` 0.29 hold). Check both before touching the epic's status.
