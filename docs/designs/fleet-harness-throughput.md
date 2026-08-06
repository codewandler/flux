# Fleet harness throughput

**Status:** proposed · Wave 2 of the Fleet harness work. Wave 1 is
[agent-evidence-scope](agent-evidence-scope.md).

## Why

Wave 1 closed the confidentiality holes. This wave is about the thing the operator originally asked
for and still does not have: **a steady end-to-end workstream, visible while it runs, that reaches
integration.**

The record from driving it by hand is the argument. Six waves were dispatched. Every one was reported
"completed" and produced no integrated work, for a different reason each time:

| wave | outcome | cause |
|---|---|---|
| 253 | all four workers died before their first tool call | the intent router's family cap applied to an authored ceiling (C-593) |
| 257 | one worker committed 661 insertions, recorded `failed` | retained-history budget destroyed a turn 0.43% over (C-595) |
| 260 | died at 0.58% over the same ceiling | same |
| 269 | 25 minutes of work discarded | a concurrent coordinator read bumped the revision; the wave persisted from a stale snapshot (C-598) |
| 275 | staged 18 edits, never finalized | the authored segment was told its actions were only captured (C-597) |
| 281/286 | committed 1072 insertions, recorded `failed` | the round budget cut the final batch; the stream parse then rejected the turn |

Each fix was real and each exposed the next. Two patterns are worth naming rather than treating as six
separate bugs:

1. **Work completes, then an accounting step destroys the record of it.** C-595, C-598 and the stream
   parse are all this. A commit existed on disk in three of these waves while Fleet reported failure.
2. **A ceiling that is not a property of the work.** `max_rounds`, the history budget, and the capture
   buffer are all fixed numbers unrelated to the assignment; raising one moves the guillotine.

Meanwhile the operator cannot see any of it: a wave writes durable state exactly twice, so the surface
has nothing to repaint for the whole wave, and the only way to ask was to interrogate the coordinator —
which, until C-598, destroyed the wave.

## Approach

Ordered by what unblocks the most.

1. **Let the worker decide when it is done** (C-603, needs C-570). An outer authored loop that runs a
   bounded segment, then checkpoints against the assignment and returns `continue`/`handoff`/`blocked`.
   Removes the arbitrary ceiling and gives progress reporting a natural place to happen. Note the
   constraint that shapes it: `ai_segment` is stateless (`current_turn: true` resets the conversation
   to one message and there is no state input), so the checkpoint summary *is* the handover contract.
2. **Make a running wave observable** (C-599, C-602). The bytes already exist —
   `guarded_agent_run_async` polls worker stdout and discards it until exit — and the transport already
   exists in both directions (`--stream-json` out, `--stream-json-input` with `steer: true` in). Fleet
   opens one half and buffers it. A bounded activity projection is already landed; the remaining work
   is the reverse channel and the live transcript view.
3. **Stop losing work to accounting** — finish the class rather than the instances: a turn that
   produced a commit must never be recorded as if it produced nothing, whatever failed afterwards.
4. **Prove integration end to end** (C-596). The integrator, its ceiling, its admission and the
   handoff gate are built and unit-tested; the delegated path has never run on real work because until
   now there was never a commit to run it on.
5. **Configuration and surfacing hygiene** (C-604, C-605): reusable direction declared once with a
   local layer that may narrow but never widen, and toolchain operations surfaced from the repository
   instead of named in shared prose.
6. **Operator-facing polish** (C-600, C-601): a coordinator transcript with a lifecycle, and
   cancellation that says it is cancelling and is bounded.

## Verification

The wave is done when a dispatched multi-story wave produces commits, hands them off, is integrated by
the dedicated agent, and reaches a green candidate on `fleet/<wave>/<repo>/integration` with
`gate.runs == 1` and local main unmodified — **and** the operator can watch it happen without asking
the coordinator anything.
