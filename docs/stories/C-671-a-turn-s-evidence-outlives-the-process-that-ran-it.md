---
id: C-671
title: "A turn's evidence outlives the process that ran it"
pillar: "Core"
status: backlog
priority: 17
epic: recovery-and-inspection-have-no-cli-so-every-failure-is-hand-driven
areas: [flux-cli]
depends_on: [C-670]
note: "the receipt is written after the turn by the process that ran it, so a killed supervisor loses the commit sha, write set and test argv even though the commit landed"
---

# A turn's evidence outlives the process that ran it

## Goal

Stop a dead supervisor from erasing the record of work that actually happened.

A turn's receipt is written **after** the turn returns, by the same process that ran it: on success the
agent record gains `status`, `session`, `last_turn` and the receipt is journalled. If that process
dies, none of it is written. There is nothing stale to notice — only silence.

Measured 2026-08-07: all ten wave-472 agents had `last_turn: null` while nine of them held finished
commits on their branches. The commits survived because git wrote them; everything the fleet knew
about those turns did not, because the fleet wrote it last.

`worker_activity` already reasons about precisely this case, and its comment names it — a recorded
supervisor that is gone settles the record. But that only lets the fleet *classify* the silence. It
cannot recover what was never written, so recovery still means a human reading worktrees.

## Acceptance

- [ ] The facts needed to recover a turn — the agent, the wave, the branch, the base, and the
      supervisor pid — are durable **before** the turn starts, not after it ends.
- [ ] A turn that ends without its supervisor is recoverable from durable state plus the worktree
      alone, with no operator reconstruction.
- [ ] `fleet status` distinguishes *no receipt because the turn is running* from *no receipt because
      the process died*, since those demand opposite responses and currently look identical.
- [ ] Evidence already captured mid-turn is not lost when the turn later fails — a failed turn's
      evidence is the most expensive kind to lose.
- [ ] The write is crash-safe rather than best-effort: a process killed between two writes leaves a
      readable record, not a half-written one.
- [ ] Failing first, a test kills a supervisor mid-turn and proves the fleet can still name the
      commit, the branch and the write set afterwards.

## Notes

- **This is what makes `C-670` trustworthy.** An automatic handoff derived from a worktree is only as
  good as the fleet's ability to know which worktree belonged to which turn — which is exactly what
  dies with the supervisor today.
- Watch the interaction with `C-659` (state.json stops carrying every agent's last turn): writing
  more, earlier, must not reintroduce the bloat that made a 12.9 MB parse look like a hang. Durable
  and small are both requirements, not one or the other.
