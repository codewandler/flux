---
id: A-109
title: "Inject memory as ContextBlocks with git-pin staleness computed at turn assembly"
pillar: Agent
status: backlog
epic: evidence-pinned-memory
design: docs/designs/evidence-pinned-memory.md
note: "stale entries are STILL injected, marked stale='true' with the reason — dropping them silently loses real knowledge; reuses the A-21-hardened, A-24-budgeted <knowledge-base> seam rather than a second injection path"
---

# Inject memory as ContextBlocks with git-pin staleness computed at turn assembly

## Goal
Get memory into the prompt with its provenance visible to the model, and make a memory whose
evidence has moved carry its own doubt rather than asserting confidently or vanishing.

## Acceptance
- [ ] Memory entries render as `ContextBlock`s through the existing `render_knowledge_blocks`
      (`flux-core/src/context.rs`) — no second injection path.
- [ ] `meta` carries the citation as tag attributes (`source`, `learned`, `sha`), so provenance is
      visible in the prompt, not only via `flux memory show`.
- [ ] Staleness is computed at turn assembly and **never cached**: `git rev-parse HEAD` (via the
      helper at `flux-runtime/src/context.rs:133`); if `HEAD != pin.sha`, one batched
      `git diff --name-only <sha>..HEAD -- <paths>` across all candidate entries. One `rev-parse`
      plus at most one `diff` per turn — asserted by a test counting git invocations.
- [ ] **A stale entry is still injected**, carrying `stale="true"` and a `stale-reason` naming the
      changed paths. **Failing-first test**: pin a memory to a file, change and commit that file,
      assert the block renders stale *and is present*.
- [ ] An entry with no `GitPin` carries neither attribute — it never claims freshness it cannot back.
- [ ] A memory body containing a literal `</knowledge-base>` cannot break out of its block —
      re-pins the A-21 property at this new call site (do not assume inheritance; test it here).
- [ ] Memory blocks respect the A-24 byte-budget accounting, with the omission marker counted
      against the budget — a test asserts `len <= cap` including header and marker.
- [ ] Selection for v1 is scope + recency under the budget. No embedding ranking (see design
      non-goals).

## Progress
- Not started.

## Notes
- Design: [evidence-pinned-memory.md](../designs/evidence-pinned-memory.md).
- Blocked by A-107 (needs the projection); does not need A-108 to be testable — inject fixture
  entries.
- The "still inject, marked" decision is the epic's central behavioural call. If a reviewer pushes
  to drop stale entries instead, the counter-argument is in the design: stale ≠ false, and dropping
  silently loses real knowledge while teaching nobody anything.
