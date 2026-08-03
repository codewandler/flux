---
id: C-443
title: "Zero `Compacted` rows in 112,114 events — does compaction ever actually fire?"
pillar: Core
status: done
design: docs/designs/docs-completeness.md
epic: docs-completeness
areas: [flux-flow, flux-events]
note: "⚠ found by A-145 while sweeping the event store for a real-run fixture. Not a docs problem: either the threshold is never reached, compaction is effectively disabled, or it fires without recording. Gates C-441, because a page describing behaviour nobody has observed is documentation of an intention"
---

# The feature with no instances

## Goal

Find out whether history compaction fires in real use, and make the answer visible.

## The finding

[A-145](A-145-a-real-run-as-the-mock-fixture.md) swept the local event store to build a real-run
fixture and reported **zero `Compacted` rows in 112,114 events** — enough that it could not construct a
compaction fixture at all, and had to leave `Compacted` in its *absent* column.

The machinery exists: `compact_threshold_chars` on the engine (`0` disables), `maybe_compact` in the
loop host, `EventKind::Compacted { messages }` in the log, and `FLUX_COMPACT_CHARS` in the config
reference.

Three possibilities, and they need different fixes:

1. **The threshold is never reached** in practice — sessions end first. Then the default is arguably
   wrong, and the docs should say reaching it is rare.
2. **Compaction is effectively disabled** — a default of `0`, or a path that never calls
   `maybe_compact`. Then it is a dormant feature.
3. **It fires and does not record.** ⚠ The worst case: history is being replaced with **no durable
   evidence that it happened**, which would silently corrupt every replay, export and reconstruction of
   an affected session.

## The answer: **possibility 1**

Compaction is wired, armed by default, and records correctly. It has never fired here because the
sessions in the store are far too small to reach the threshold — and the one session that did reach it
reached it on its *last* message, after which no turn remained to compact.

### The default is non-zero on every surface

`DEFAULT_COMPACT_THRESHOLD_CHARS = 48_000` (`crates/flux-agent/src/lib.rs:161`) — used by the SDK and
by served/agentic agents via `compact_threshold_for_decl` (`crates/flux-app/src/app.rs:1813`). The CLI
now reads that same constant in `compact_threshold()` (`crates/flux-cli/src/execution.rs:342`),
honouring `FLUX_COMPACT_CHARS`. Possibility 2 is out: nothing defaults to `0`.

### The call path is automatic, not just the `/compact` command

`FlowEngine::run_turn_*` → `run_turn_lifecycle` → `execute_turn_program`, and `TurnProgram::Adaptive`
calls `compaction_attempt` **before** the loop body every turn (`crates/flux-flow/src/engine.rs:938-948`).
`Adaptive` is the default program whenever no authored flow is given (`engine.rs:875`), so it is the path
every ordinary CLI/TUI/SDK turn takes. `maybe_compact` (`engine.rs:1611`) is a thin wrapper over the same
`compaction_attempt`, exposed for the REPL `/compact` command (`flux-cli/src/session.rs:986`) and the TUI
(`flux-tui/src/lib.rs:4677`).

The gates, in order: threshold `0` → no-op · fewer than 4 messages → no-op · `total <= threshold` → no-op ·
`ValidHistory::snap` finds no legal split → no-op. `total` is `sum(len(serde_json::to_string(message)))`
over the projected conversation.

### Possibility 3 is structurally impossible

Compaction replaces the history by calling `SessionLog::rewrite` (`engine.rs:1716`), and `rewrite` *is*
the `Compacted` writer — `self.commit(NewEvent::compacted(history.into_inner()), …)`
(`crates/flux-events/src/session_log.rs:187-190`). Replacing without recording is not a path that exists:
the record is the replacement mechanism. Every other history-replacing caller in the tree (fork, what-if,
replay, the CLI's history rewrite) goes through the same `rewrite`, so all of them record too.

The `Compacted` event carries the replacement messages (`EventKind::Compacted { messages }`), and the
superseded `Message` events stay on the append-only stream — so the log answers **both** "what is the
history now" and "what was replaced". Pinned by the new test.

### The store: the threshold is essentially never reached

`~/.flux/events.db`, read-only sweep, 112,114 events / 1,465 streams:

| measure | value |
|---|---|
| `compacted` rows | **0** |
| `message` rows | 3,376 |
| streams with ≥1 message | 1,126 (339 have none at all) |
| …of those, with ≤2 messages (one-shot `flux run`) | **957 (85%)** |
| …with ≥4 messages (past the `len < 4` gate) | 167 |
| mean conversation size of those 167 | 5,056 chars — **9% of the threshold** |
| streams that ever exceeded 48,000 chars | **1** |

The single crossing is `s_368`: 38 messages, 50,755 chars. Its running total crosses 48,000 only at its
**final** message (`stream_seq 877`, which is also the stream's `MAX(stream_seq)`). Compaction is checked
at the *start* of the next adaptive turn, and there was no next turn. Its absence is expected behaviour,
not a miss.

Runner-up is 37,921 chars — 79% of the threshold, and never checked against it because the session ended.

### ⚠ Is the store representative? No — and that bounds the conclusion

It is not a sample of heavy interactive coding sessions. 85% of its sessions are two-message one-shot
invocations, plus mock/eval traffic (197 streams on model `mock`, 46,520 `run` and 46,875 `observation`
events dwarfing the 3,376 messages). **"Zero `Compacted` rows" measures this workload, not the
threshold.** What the data does support: on short and one-shot use, compaction never fires, and that is
correct. What it cannot support: a claim that the threshold is wrong for a heavy interactive user. The
one long session that existed *did* cross it, which is weak evidence the threshold is reachable rather
than unreachable. Settling it needs a store from sustained interactive use, which nobody has yet.

### The default stays at 48,000 — reasoning

- **Not raised.** The constant exists (A-22) to stop served/SDK agents on one persistent session growing
  unbounded into a provider context-window error. Raising it weakens that guard, and no evidence here
  asks for it.
- **Not lowered.** Lowering makes compaction fire on sessions under no memory pressure, spending a
  provider call and discarding fidelity for nothing. The 167 multi-turn sessions average 5,056 chars; a
  threshold low enough to fire on them would compact almost every real session.
- **The zero count is not evidence against the value.** A threshold crossed by 1 of 1,126 sessions on a
  workload of 85% one-shot runs is a statement about the workload.
- **Open, deliberately not settled here:** 48,000 chars ≈ 12k tokens is a *uniform* budget regardless of
  the model's actual context window — arguably mis-scaled against a 200k/1M-token model, where it errs
  toward compacting early. Re-scaling it to the model's window is a tuning change needing its own
  evidence and its own story: filed as
  [C-462](C-462-compaction-threshold-is-context-window-blind.md).

### For [C-441](C-441-context-management-doc.md) — the honest wording

> flux compacts a session's history once the serialized conversation passes `FLUX_COMPACT_CHARS`
> (default **48,000 characters**, roughly 12k tokens; `0` disables it). The check runs at the start of
> each turn, and needs at least 4 messages. When it fires, the older messages are summarized into one
> synthetic `[summary of earlier conversation]` message and the history is replaced — durably, as a
> `Compacted` event that carries the replacement. The superseded messages remain in the event log, so a
> replay or export can still see what was replaced.
>
> **In practice this rarely fires.** A sweep of a 112,114-event local store found zero compactions: most
> sessions are one-shot runs, and the average multi-turn session is under 10% of the threshold. Expect
> compaction only in a sustained session of roughly 35-40 substantive messages.

## Acceptance

- [x] **Failing-first**: a test driving a session past the threshold and asserting a `Compacted` event
      is recorded — failing at the merge base if possibility 3 holds.
      → `an_adaptive_turn_past_the_threshold_records_a_durable_compacted_event`
      (`crates/flux-flow/src/engine.rs:4701`). Possibility 3 does **not** hold, so it passes at the merge
      base; it is a pin, and it dies when the `compaction_attempt` call is removed from the adaptive path.
- [x] Which of the three it is, stated with evidence — the default value, the call path, and a check of
      whether any session in a store has ever crossed the threshold. → **Possibility 1**, above.
- [x] ⚠ **If it is possibility 3, that is a correctness bug** → it is not. `SessionLog::rewrite` is the
      sole history-replacement path *and* the sole `Compacted` writer, so replay, export and C-422's
      reconstruction are reading a complete past.
- [x] The default threshold is either justified or changed, with the reasoning recorded. → justified,
      unchanged at 48,000; reasoning above and summarized at the constant.
- [x] The answer is handed to [C-441](C-441-context-management-doc.md) in a form it can document
      honestly — including "this rarely fires". → the block above, written to be lifted.

## Notes

- ⚠ A-145's sweep is one machine's store and one user's habits. Confirm the store is representative
  before concluding the threshold is wrong for everyone — but a 112k-event store with zero instances is
  a strong signal from *somewhere*.
- Feeds [C-422](C-422-the-render-projection.md), which has "pre- or post-compaction view?" as an open
  question it cannot currently settle against any real data.
- The Notes on `EventKind::Compacted { messages }` say it carries the replacement messages, so the log
  *can* answer "what was replaced" — worth confirming that survives whatever this finds.

## Progress

- Filed 2026-08-02 from A-145's event-store sweep.
- 2026-08-02 — investigated. **Possibility 1**: the threshold is never reached, because the store is 85%
  one-shot sessions; the sole session that crossed it crossed on its final message. Compaction is armed
  by default on every surface and records correctly; possibility 3 is structurally impossible because
  `SessionLog::rewrite` is both the only history-replacement path and the only `Compacted` writer.
- Added `an_adaptive_turn_past_the_threshold_records_a_durable_compacted_event` — the first test on the
  **automatic** (`TurnProgram::Adaptive`) compaction path, and the first to assert the durable
  `Compacted` *event* rather than only the resulting projection. The pre-existing
  `compaction_never_writes_a_user_after_user_history` covers the `/compact` entry point and asserts only
  the projection, so neither half of the "what is it now / what was replaced" guarantee was pinned.
- Default left at 48,000; justification recorded above and at `DEFAULT_COMPACT_THRESHOLD_CHARS`.
- **For [C-422](C-422-the-render-projection.md)** — this retires the prior question, not the design one.
  *Retired:* whether a reconstruction can trust the past it reads. It can. Because `SessionLog::rewrite`
  is simultaneously the only history-replacement path and the only `Compacted` writer, there is no such
  thing as a silently truncated stream to reconstruct from. *Still open:* "pre- or post-compaction view?"
  — a design choice, and still unsettled by real data, since there is no compacted session anywhere in
  the store to render. What C-443 adds is that **both** views are reconstructible: the `Compacted` event
  carries the replacement, and the superseded `Message` events are never removed.
- Spun the one finding this story declined to act on out into
  [C-462](C-462-compaction-threshold-is-context-window-blind.md) (subsequently done): it kept the
  threshold as an intentional fixed history budget rather than a model-window fraction.
- Next: C-441 lifts the wording block above; nothing else blocks it.
