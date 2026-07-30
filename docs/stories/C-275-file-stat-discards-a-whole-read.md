---
id: C-275
title: "`file_stat` reads the whole target a second time and throws it away"
pillar: Core
status: ready
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

- [ ] `file_stat` no longer reads the file's entire contents solely to discard them. Verified at
      `crates/flux-tools/src/extra.rs:96-107`: `read_file_bytes(path)` is awaited and the result passed
      to `.map(|_| "(mode unavailable)".to_string())` — the bytes are never used.
- [ ] The dead `mode_str` binding and its `let _ = mode_str;` line go with it (`:107`). Its trailing
      comment promises "we surface it as a note below" and **no such note exists** — verified: the
      emitted JSON is `{path, size_bytes, line_count, mtime_unix}` and the view has four lines, neither
      mentioning mode (`:108-118`).
- [ ] Either the op reports a mode honestly or it says nothing about mode at all. A comment claiming a
      note that is not emitted is the actual defect here — worse than an absent field, because it tells
      the next reader the case is handled.
- [ ] **The author's instinct is preserved, and this is the constraint that matters.** The comment at
      `:102` records why `std::fs::metadata` was declined: on the raw caller-supplied string it would
      escape the guarded jail. Any fix must go through the guarded surface — and note that a
      `host_path_identity`-style physical reduction now exists on `System`, which did not when this was
      written. Do **not** reach for `std::fs`; `scripts/check-no-direct-io.sh` will refuse it anyway.
- [ ] A test pins whichever contract is chosen, so a future edit cannot silently reintroduce a
      read-and-discard. Name it in this story.
- [ ] Full gate green, and the finding is marked closed in
      [`reviews/2026-07-30-security-assurance-closure.md`](../../reviews/2026-07-30-security-assurance-closure.md)
      so the next reviewer sees it closed rather than re-deriving it — which is the whole point of that
      artifact.

## Progress

- (not started)

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
