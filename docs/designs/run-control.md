# Design: Pause and resume a live run

**Status:** proposed · **Pillar:** Agent · **Stories:** [A-140](../stories/A-140-pause-a-live-run.md) · [A-141](../stories/A-141-what-pause-means-for-an-effect-in-flight.md)

## Why

Watching an agent work is only half of control. The other half is being able to say *stop* — to read
what it is about to do, to think, to show someone, to intervene — and then *continue*.

It is the natural companion to the loop thread ([agent-loop-visibility](agent-loop-visibility.md)): a
view you can watch but not halt is a dashboard, and a run you can halt but not see is a black box.
Together they are control.

And it is the foundation the [interactive debugger](interactive-debugger.md) needs. A debugger is a
pause with inspection and mutation attached; without a well-defined suspension point there is nothing
to attach them to.

## ⚠ flux already suspends — three times — and none of them is this

This is the important framing, because it decides whether this is a small story or a deep one.

1. **`await` / durable journeys.** A journey suspends on an event and resumes later, durably.
   *Author-declared*: the program says where it suspends.
2. **`ParkedAsk`** (`crates/flux-app/src/park.rs`, touched by C-415). A journey that asks a human parks
   and resumes when a reply correlates. Again *program-declared*.
3. **Approval.** Every effect stops at the envelope for a human decision. That is a suspension at a
   *known boundary* — the one place the runtime already halts and waits for a person.

All three are **cooperative and declared in advance**. What this epic adds is *"stop now, wherever you
are, because I said so"* — an **operator-initiated** halt at a point nobody declared. That is a
different problem, and pretending it is the same one is how this ships broken.

**The good news:** approval already proves the runtime can hold a turn mid-flight while a human decides,
without losing state or leaking resources. That is the mechanism to extend, not to invent.

## Approach

### A-140 — pause a live run, and continue it

A hotkey in the TUI that suspends the loop at the next safe boundary and resumes it. The word doing all
the work is **boundary**: pausing between steps is tractable; pausing *inside* a running tool is not the
same feature.

### A-141 — what pause means for an effect in flight

The story that decides the semantics, and the one that makes the feature trustworthy or not. A pause
pressed while an HTTP request is in flight, a subprocess is running, or a model is streaming cannot
un-send any of it. ⚠ The honest answer is almost certainly *"pause takes effect at the next boundary,
and the view says what is still finishing"* — and saying that plainly beats implying a stop that did not
happen.

## Alternatives considered

- **Ctrl-C / kill and restart.** Exists, and it is what people do now. Rejected as sufficient: it
  discards the run, which is the opposite of intervening in it.
- **Only pause at approval points.** Nearly free — the machinery is already there. Rejected as the
  whole answer: an autonomous run under `--yes` has no approval points, which is exactly the run you
  most want to be able to halt. Worth shipping as a first increment, if labelled honestly.
- **Suspend the OS process (SIGSTOP).** Rejected: it freezes the world including the UI, times out
  network peers, and holds every resource open. It looks like pause and behaves like a hang.

## Risks & open questions

- ⚠ **A pause that does not pause.** If the hotkey reports stopped while effects continue, the feature
  is worse than absent — it invites the operator to relax at exactly the wrong moment.
- ⚠ **Pause is not a safety control.** It must not be documented as a way to stop a dangerous
  operation; the approval envelope is that, and it runs *before* the effect. Confusing the two would
  weaken a real guarantee with a soft one.
- **Timeouts keep running while paused.** A paused run whose provider connection ages out has not
  really been paused. Whether the pause clock stops is a real decision.
- **Open:** whether pause is per-run or global when sub-agents are in flight. A fleet where the parent
  pauses and children continue is confusing; stopping all of them is a distributed problem.
- **Open:** whether the pause is durable — survives a restart — or purely in-session. Durable makes it a
  journey-shaped feature; in-session keeps it a TUI feature.

## Acceptance / done

- A hotkey pauses a live run at the next safe boundary and resumes it, with no lost state.
- What "safe boundary" means is documented, and what is still finishing is visible during a pause.
- Pause is never presented as a substitute for the approval envelope.
- Nothing about it applies backpressure to a run that is *not* paused.
