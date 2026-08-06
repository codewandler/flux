# Design — The fleet improves its own harness in a dedicated worker group with an independent integration target

## Why

Harness defects are currently fixed by hand while the fleet waits, and every one of them blocks the
delivery of ordinary feature work behind it. There are **87 fleet-related stories** on the board; the
handful that gate the pipeline are the reason the rest cannot move. Meanwhile the fleet is perfectly
capable of implementing most of them: it has already delivered harness work end to end.

Two things argue for a *separate group with a separate integration target* rather than just dispatching
harness stories into the normal queue:

1. **Independent merge cadence.** A harness fix is worth landing the moment it is green, because it
   unblocks everything else. Feature work can accumulate for a batched, gated snapshot. Sharing one
   integration branch forces the fast thing to wait for the slow one.
2. **Blast-radius separation.** A bad harness change breaks the machine that runs every other story. It
   should be gated and reviewed on its own branch, not mixed into a wave whose other stories are
   innocent.

## Approach

A second worker group, bound to the same repository but with its own integration target, so harness
candidates assemble and gate independently of feature candidates.

**A worker cannot be broken by the fix it is writing.** Workers execute the *installed* binary, and the
change under construction lives only in a worktree until an operator installs it. So self-improvement is
safe by construction — the dangerous step is installation, which stays an explicit operator action with
its own gate. That property should be stated in the design rather than assumed, because it is the whole
reason this is not circular.

**One harness story per wave, until overlap is solved.** Nearly every harness story touches
`board_fleet_cmd.rs`, and integration refuses a wave in which two stories wrote the same file. A
dedicated group that dispatches three harness stories at once would deadlock itself on the first
integration attempt. This is the same class of problem as the shared-ledger collision that made wave-346
unintegrable: two stories, each correctly told to update `CHANGELOG.md`, produced a wave that could never
integrate. Until a wave can integrate per-story or merge ledgers, width for harness work is one.

**Which stories belong to the fleet, and which do not.** The division is not by difficulty, it is by
whether a wrong answer disables the pipeline that would deliver the fix:

- *Operator-held*: anything on the dispatch, handoff, integration or confinement path — a mistake there
  stops the fleet from being able to deliver anything, including the correction.
- *Fleet-held*: additive and observability work — worker transcripts, activity streaming, cancellation
  state, config layering, surface panes, dry-run validation.

## Stories

- A second worker group in one fleet, with its own integration target and its own gate.
- Wave width per group, so a harness group can be pinned to one story while feature groups run wide.
- Integration assembles per story or merges append-only ledgers, so a shared-ledger edit cannot make a
  wave unintegrable.
- The group's candidates carry a distinct tag namespace, so accepted harness work is findable separately
  from feature work.
- A documented split of which harness stories are operator-held, checked by a test so the list cannot
  drift silently.
