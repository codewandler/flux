---
id: C-430
title: "Distil a recorded exploration into a flow — keep the path that worked, drop the trial and error"
pillar: Core
status: ready
priority: 12
design: docs/designs/explore-then-freeze.md
epic: explore-then-freeze
areas: [flux-cli, flux-flow, flux-events]
note: "the epic's core verb. Raw material exists — since L-38 every accepted plan records parseable `plan_source`, redacted at record time — but nothing turns a recorded SESSION into a saved FLOW: `flux flow` has list and run, no save. ⚠ Do not emit `e<N>` refs; C-431 owns why"
---

# The path that worked, without the wandering

## Goal

One command turns a recorded session into readable, committable Flux-Lang containing the sequence that
succeeded — with the backtracking removed.

## What exists and what does not

- **Exists**: since L-38 every accepted plan records **parseable** `plan_source`, redacted at record
  time; `flux export` re-renders it through the same redactor
  (`crates/flux-cli/src/export_cmd.rs:277`). The run trace records which ops ran and which errored.
- **Does not exist**: any path from a recorded session to a saved flow. `flux flow` offers `list` and
  `run` (`crates/flux-cli/src/args.rs`); there is no `save`.

So this is not a recording feature — the recording is already there. It is a **selection and emission**
feature.

## Acceptance

- [ ] **Failing-first**: a test distilling a fixture session that contains at least one failed attempt,
      asserting the emitted flow contains the successful path and **not** the failed one — failing at
      the merge base.
- [ ] Output is native Flux-Lang text that **parses, lowers, and formats** — it must survive the same
      gate the corpus does, or it is a transcript dump with a `.flux` extension.
- [ ] ⚠ **Emits durable locators, not `e<N>` refs.** See [C-431](C-431-durable-locators.md): refs are
      assigned per live session and mean nothing in a fresh one. A distiller that emits them produces
      scripts that break on the next deploy. If C-431 has not landed, this story emits refs **and
      refuses to write the file without an explicit override flag naming the limitation** — it must not
      quietly produce brittle artifacts that people commit.
- [ ] ⚠ **Provenance survives.** The emitted flow names the session it came from. Dropping the failures
      is the point and the risk: a script with no record of why it is shaped as it is invites someone
      to "simplify" it back into the thing that did not work.
- [ ] Readable output. A distilled flow a human will not read is one nobody will maintain, and it
      defeats the purpose of emitting a language instead of a blob.
- [ ] The verb is named per the repo's explicit-subcommands rule — no implicit default-run — and the
      name says what it does. `flux flow save <session>` and `flux distil` are both candidates; pick one
      and say why.
- [ ] Full gate green.

## Notes

- ⚠ **Open, and worth deciding before coding**: whether the distiller is itself model-driven. Selecting
  the successful subsequence is partly mechanical (which ops errored) and partly semantic (which
  succeeded but were dead ends — an op can return 200 and still be a wrong turn). A model-driven
  distiller is more capable and makes *freezing* non-deterministic. That is acceptable — the artifact
  is then reviewed and committed by a human — but it must be a **stated** choice, not a drift.
- ⚠ The model's judgement of "the happy path" is inherited wholesale. If the agent succeeded by
  accident, the frozen script enshrines the accident. [C-433](C-433-a-frozen-script-asserts.md) is
  where that becomes detectable.
- `plan_source` is redacted at record time — do **not** assume that makes the output safe to commit;
  [C-432](C-432-browser-credentials-never-come-from-the-prompt.md) covers the case the redactor never
  knew about.
- Not browser-specific in principle. Browser exploration is the motivating case and the hardest one; if
  the design falls out general, say so, but do not widen the scope to chase it.

## Progress

- Filed 2026-08-01 with the explore-then-freeze epic.
