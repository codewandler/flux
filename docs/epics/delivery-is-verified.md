---
id: E-31
title: "Delivery Is Verified"
---

# Delivery Is Verified

## Why

Filed by the C-742 migration: 24 stories carried `epic: delivery-is-verified` with no document behind it.

## Success criteria

- [ ] Every fleet verb that reports an effect names the artifact that proves it — a sha, a path or a
      ref — and a run that changed nothing reports zero rather than what it considered
      (`fleet reclaim`, `apply`, `integrate`, `drive`).
- [ ] A worker turn's reported status and its delivered commit cannot disagree: a turn that left work
      uncommitted is reported as such rather than closed (C-722, C-725).
- [ ] A wave is applied only when the canonical ref actually contains its commits, asserted by a test
      that fails when the ref is behind (C-721).
- [ ] A story is dispatched only against a contract a tool can read, and closed only against criteria
      that are ticked (C-736, C-737).

## Exit criteria

- [ ] Every story carrying `epic: delivery-is-verified` is `done` (`flux board epics --slug delivery-is-verified`).
- [ ] Every success criterion above is ticked.
