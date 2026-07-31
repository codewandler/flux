---
id: C-384
title: Record success-time, content-anchored write receipts
pillar: Agent
status: backlog
epic: agent-change-recovery-and-provenance
design: docs/designs/agent-change-recovery-and-provenance.md
note: "the only path-anchored durable record is the PRE-dispatch tool_call observation, so a failed edit is indistinguishable from a successful one; no blob or hunk identity, and the model-facing reader sees the in-process log only"
---

# Record success-time, content-anchored write receipts

## Goal

Make "which changes are mine" answerable from durable evidence rather than from conversational
memory.

## Acceptance

- [ ] A successful `write`/`edit`/`patch` dispatch emits an observation carrying
      `{path, post_image_hunk_ids, session_id, turn_id}`, reusing the existing deterministic
      `hunk_id(path, ordinal, body)` (`crates/flux-tools/src/lib.rs:3358-3377`).
- [ ] A failed dispatch records the existing pre-dispatch marker and **no** write receipt, so the
      two are distinguishable.
- [ ] The receipts survive a resume — the model-facing reader must not be limited to the in-process
      `EvidenceLog` (`crates/flux-tools/src/evidence.rs`).
- [ ] Failing-first: two edits to one file across two turns yield two receipts with distinct hunk
      ids; a failed edit yields none.

## Progress

- 2026-08-01 — filed from validation of GIT-02. "Not machine-verifiable" overstates the finding —
  path-level attribution exists; it is insufficient for disambiguating a mixed-hunk file.

## Notes

- `finish_dispatch` (`crates/flux-runtime/src/lib.rs:3243`) is where the success-time observation
  belongs; it currently adds no outcome observation at all.
