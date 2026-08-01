# Design: The interactive debugger — stop, inspect, change a value, continue

**Status:** proposed · **Pillar:** Agent · **Stories:** [A-142](../stories/A-142-inspect-a-paused-run.md) · [A-143](../stories/A-143-change-a-value-and-continue.md)

## Why

Debugging an agent today means reading a transcript after the fact and running it again. Being able to
**stop it mid-run, look at what it is holding, change a value, and let it continue** turns an agent from
something you observe into something you work with — and it is only possible because flux's runs are
programs rather than transcripts. An agent whose contract is its conversation has no variables to
inspect and no execution point to resume from.

## ⚠ flux already does this offline, and the machinery is shipped

This is the finding that changes the size of the epic.

**`flux fork <session> --at N --inject <json>`** (A-46, shipped) already means: *replay the run
hermetically up to statement N, substitute a different value for that statement's result, then run the
rest live through the real approval envelope.* Its siblings `--edit` (continue with an edited flow) and
`--replan` (re-enter the adaptive agent from the forked state) round it out, and `flux replay` (A-45) and
`flux diff` (C-44) provide inspection and comparison.

Stop at a point · change a value · continue from there — **the debugger's semantics already exist**, for
*recorded* runs, as a CLI.

**So the gap is narrower and sharper than "build a debugger":**

| | recorded run (today) | live run (this epic) |
|---|---|---|
| stop at a point | `--at N` | needs [run-control](run-control.md) |
| inspect state | `flux replay`, `flux export` | needs a live read |
| change a value | `--inject` | needs a live write |
| continue | tail runs live | needs resume |

⚠ And the difference is not cosmetic. A **forked replay holds nothing**: the prefix is hermetic, no side
effects, so injecting a value is free. A **live paused run holds real resources** — an open connection,
a spawned process, a half-written file — and a value changed underneath it can contradict effects that
already happened. That is the actual design problem, and it is why this epic is not "wire the TUI to
`--inject`".

## Approach

### A-142 — inspect a paused run

Read-only first, deliberately. See the variables and state a paused run is holding, rendered through the
same vocabulary the plan and thread views use. Read-only is genuinely useful on its own and cannot
corrupt a run — and it is the half that makes the *other* half reviewable.

⚠ Inspection is a **disclosure** surface: a paused run's state includes tool outputs and may include
secret material. It must route through the `Redactor`, and — since C-339 found redaction failing *open*
in this codebase — the failure path needs a test, not just the happy one.

### A-143 — change a value and continue

The write half. ⚠ **A changed value must be recorded as an intervention, not silently applied.** A run
whose evidence log does not show that a human altered its state has lost the property that makes flux's
runs auditable — and that property is worth more than the feature. `--inject` already has precedent
here: a fork is a first-class replayable, diffable session, not an edit of the original.

## Alternatives considered

- **A DAP (Debug Adapter Protocol) server.** Editors get a debugger UI free, and flux already ships an
  LSP. Rejected as the *starting* point — DAP's model is threads, stack frames and breakpoints, which
  fits a stepping language runtime better than an agent loop — but it is the obvious second surface
  once the semantics are settled, and the semantics should not be shaped so as to preclude it.
- **Only debug recorded runs (extend `fork`).** Cheapest by far, since it is nearly shipped: a TUI over
  `replay`/`fork`/`diff`. Rejected as the whole answer because the ask is live intervention — but it is
  a genuinely valuable increment and may be most of the demo value.
- **Breakpoints in Flux-Lang source.** Attractive and probably wanted eventually. Rejected for now:
  authored-source breakpoints need a stable source↔runtime mapping this epic would otherwise have to
  build first.

## Risks & open questions

- ⚠ **Mutation versus auditability.** flux's evidence chain is a core guarantee. A human-changed value
  must appear in it as an intervention, or the log now lies. This is the invariant that must not be
  traded for ergonomics.
- ⚠ **Inspection is disclosure.** See A-142. In a shared or recorded setting — a demo, a screenshare —
  the debugger pane is a live view of everything the run is holding.
- ⚠ **A changed value can contradict effects that already happened.** Setting `$total = 5` after the
  invoice was sent for 500 does not un-send it. The debugger must not imply otherwise.
- **Open:** whether a changed value forks the session (matching `flux fork`'s existing "a fork is a
  first-class session" model) or mutates it in place. Forking is far more honest and costs a session
  boundary mid-run.
- **Open:** which state is even addressable. Flux-Lang locals, the context pack, the provider ledger and
  the tool registry are all "state", and they are not equally safe to expose or to change.

## Acceptance / done

- A paused run's state can be inspected in the TUI, redacted, without affecting the run.
- A value can be changed and the run continued, with the intervention recorded in the evidence chain.
- The debugger never presents a changed value as though it un-did an effect that already happened.
- Nothing here weakens the approval envelope: a continued run's effects go through it exactly as before.
