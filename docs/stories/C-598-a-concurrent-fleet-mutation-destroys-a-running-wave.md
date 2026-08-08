---
id: C-598
title: "Stop a concurrent Fleet mutation from discarding a running wave's results"
pillar: Core
status: done
areas: [flux-cli]
note: "asking the coordinator anything mid-wave lost every worker receipt; the wave loop persisted from a snapshot taken before the workers ran"
---

# Stop a concurrent Fleet mutation from discarding a running wave's results

## Goal

A wave that ran for twenty-five minutes must not lose every receipt because something else touched
Fleet state while it worked. Reading status from the coordinator is a normal operator action, not a
reason to discard completed work.

## Acceptance

- [x] The wave loop refreshes durable state after its workers join and before applying their
      outcomes, so the results are written onto current state rather than a pre-run snapshot. Mirrors
      `execute_and_record_agent_turn`, which already refreshes before recording.
- [x] `cargo test --workspace --lib` green (40 suites); clippy `-D warnings` clean; fmt clean.
- [ ] A regression test drives a wave with an interleaved state mutation and asserts the receipts
      survive. *(Not yet written — the wave loop needs a seam that does not spawn real subprocesses.)*

## Progress

- Fixed. `FleetAction::Run` re-reads state between `outcomes.extend(...)` and the loop that applies
  them.

## Notes

- Reproduced live, and self-inflicted, which is the clearest evidence of how easy it is to hit.
  Wave-269 (`flux/C-562`) was dispatched and ran for ~25 minutes. Mid-wave, a **read-only status
  request** was sent to the coordinator — `flux fleet message main "Report status only… Do not
  dispatch, cancel, integrate, or mutate any Board or Fleet state."` That turn still bumps the
  revision (271 → 273), because delivering and completing an agent turn are themselves recorded
  mutations. When the wave finished:

  ```
  error: conflict/precondition: fleet state compare-and-set failed:
         stale fleet revision 271; current revision is 273
  ```

  The `bail!` fires *before* anything is persisted, so the entire result was discarded: no receipt,
  no `runtime_session`, the worker left permanently `working`, and the wave stuck at `accepted`.
- The single-turn path was never exposed to this. `execute_and_record_agent_turn` refreshes state
  immediately after its turn returns; the wave loop had no equivalent, and instead built its final
  persist from the snapshot captured before the workers were even spawned.
- Severity is higher than it looks: **the coordinator TUI is the intended operating surface**, and
  any turn taken there — including a deliberately read-only one — increments the revision. So the
  documented way to ask "how is the wave going?" was also the way to destroy it.
- The instruction "do not mutate any Board or Fleet state" was obeyed by the model exactly. The
  mutation was the *host's* own turn bookkeeping, not the coordinator's doing.
- Related: [C-599](C-599-fleet-work-is-unobservable-while-it-runs.md) — the reason an operator
  reaches for a status turn at all is that a running wave is otherwise invisible.
