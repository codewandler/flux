---
id: C-668
title: "An unscouted item cannot become ready"
pillar: "Core"
status: backlog
priority: 14
epic: scouting-makes-the-backlog-schedulable
areas: [flux-cli]
depends_on: [C-667]
note: "ready already refuses without a priority; a scout stamp is one more clause in a rule that exists, not a new concept"
---

# An unscouted item cannot become ready

## Goal

Make scouting a precondition for scheduling, so an item whose shape nobody knows cannot be picked up.

`backlog → ready` already refuses when an item has no priority. This adds a second clause to that
same precondition: no fresh scout stamp, no `ready`. Reusing the existing rule matters — a new
parallel gate somewhere else would be a second thing to keep in sync with the state machine.

**Deliberately last in the epic.** Turning this on before `C-667` clears the debt would freeze the
backlog: every item would need scouting before it could move, and the thing that scouts would be
queued behind the gate it feeds.

## Acceptance

- [ ] `flux board transition <id> ready` refuses an item with no scout stamp, and the refusal names
      the missing stamp and the verb that produces one.
- [ ] The refusal is a `conflict/precondition`, consistent with how the missing-priority refusal is
      already classed.
- [ ] A stale stamp is refused on the same terms as a missing one, so an item cannot be made `ready`
      against a scan of a body it no longer has.
- [ ] The rule is stated where an operator reads it — `board skill` and the boards documentation —
      because a precondition nobody can discover reads as a bug.
- [ ] Items already `ready` are **not** retroactively invalidated. Say so explicitly: this gate
      governs new work, and the existing backlog is `C-667`'s job. A gate that silently unmakes 711
      ready items would stop the fleet dead.
- [ ] Failing first, a test proves an unscouted item is refused `ready`, a scouted one is admitted,
      and a stamped-then-edited one is refused again.

## Notes

- The `--force` question is deliberately not answered here. If an escape hatch is needed it should be
  its own decision with its own reasoning, not a flag added quietly alongside the gate that exists to
  prevent exactly what the flag would allow.
