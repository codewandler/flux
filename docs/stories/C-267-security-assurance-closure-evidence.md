---
id: C-267
title: "Record C-186's closure evidence against the 2026-07-29 baseline"
pillar: Core
status: ready
priority: 2
epic: security-assurance
design: docs/designs/security-assurance.md
note: "C-186's last unchecked acceptance bullet — every child is done, but nothing records that findings 1-4 and classification trust are closed, so the next review re-derives instead of verifying"
---

# Record C-186's closure evidence against the 2026-07-29 baseline

## Goal

C-186 exists because the 2026-07-29 external review rated flux's security *architecture* 8/10 and its
*assurance* 5/10, and its explicit promise was to "leave a trail that lets the next review verify the
closure instead of re-deriving it." Every child story is now `done`, but that trail was never written:
the epic's last acceptance bullet — a re-run that marks findings 1–4 and classification trust **closed
with evidence**, diffed against the baseline — is still open. Without it the epic's own deliverable is
the one thing it did not deliver, and the next reviewer starts from zero.

## Acceptance

- [ ] Each of the 2026-07-29 findings (1–4 plus classification trust) is mapped to the commit, test
      name and file:line that closes it — evidence, not assertion. A finding whose closure cannot be
      pointed at stays **open** and is said to be open.
- [ ] The mapping is verified against the tree at the current release, not against the story text.
      A child marked `done` whose claimed control is absent or unreachable in the shipped tree is a
      finding in its own right and gets a new story rather than a tick.
- [ ] The result lands as a dated artifact under `reviews/`, in the shape the existing review
      artifacts use, diffed explicitly against the 2026-07-29 baseline so the delta is readable.
- [ ] C-186's three stale acceptance boxes are ticked, since C-187/188/189/190, C-191 and
      C-192/193/194 are all `done` — or, if any tick cannot be justified from the tree, it is left
      unticked with the reason recorded.
- [ ] C-205 is recorded as the epic's one **deliberately unclosed** child, with its actual blocker
      stated: `lru 0.12.5` is transitive via `ratatui 0.29.0`, so reaching `>= 0.16.3` requires a
      breaking `ratatui 0.30.x` upgrade, for an *unsound*-class advisory reachable only through
      `LruCache::iter_mut`, which flux never calls. An epic closing over a known-open child must say
      so out loud.
- [ ] C-186's own `status` is set from what the evidence supports — `done` only if every finding is
      genuinely closed or explicitly and defensibly deferred.
- [ ] The board's hand-written Status block is corrected: it currently claims C-186 is "nearly closed
      — C-195 and C-210 remain", and both are `done`. No generator catches that text.
- [ ] Docs coverage tests stay green (`cargo test -p flux-cli --test website_contract`,
      `cargo test -p codewandler-flux-lang --test website_in_sync`).

## Progress

- (not started)

## Notes

- Read-only over the code; the deliverable is an artifact plus ledger corrections. No behavioural
  change, so no failing-first test — the evidence mapping is the proof obligation instead.
- Three independent adversarial reviews were already run on 2026-07-30 against `cb3bb057` and are
  recorded in `bcfab0ad`; their findings became the C-255 epic, which shipped in 0.38.0. Those are an
  input to this closure, **not** a substitute for it: they targeted a newer tree than the
  2026-07-29 baseline this epic must diff against, and C-255 is a different epic with its own
  outstanding closure bullet.
- The `adversarial-review` skill is at `.agents/skills/adversarial-review/SKILL.md`; existing dated
  artifacts under `reviews/` show the expected shape.
- ⚠ Do not confuse the two epics: C-186 traces to the **2026-07-29** desk + envelope-integrity
  reviews; C-255 traces to the **2026-07-30** three-review round. Closing one does not close the other.
