---
id: A-145
title: "Replace the mocks' hand-authored flow with a real recorded run — and re-check the recommendation against it"
pillar: Agent
status: in-progress
priority: 6
design: docs/designs/agent-loop-visibility.md
epic: agent-loop-visibility
areas: [flux-tui, docs]
note: "⚠ A-144's fixture was hand-authored by the same context that then chose a layout from it — the repo's recurring defect class, evidence agreeing with its own assumptions. A real run has shapes nobody thinks to invent. ⚠ The capture is a PUBLISHING act: it comes from a real machine into a public repo, and `flux export` redacts only what the Redactor was told about"
---

# Judge the layouts on a run nobody designed

## Goal

Drive A-144's five mocks from a **real recorded flux run** instead of a hand-authored flow, and
confirm or revise the recommendation against it.

## Why the synthetic fixture is not good enough

A-144's fixture was written by hand, and its hard cases — 49 steps, 8 levels of nesting, 6 concurrent
workers — were *chosen* by the same context that then picked a layout from them. That is this repo's
recurring defect class in its most comfortable form: **the evidence agrees with the assumptions of
whoever assembled it.** A layout that survives a shape you invented to test it has proven very little;
one that survives a run nobody designed has proven something.

Real runs carry shapes a synthetic fixture does not think to include — and these are exactly the ones
that break layouts:

- wildly uneven step durations (a 40 ms read beside a 90 s model call);
- a tool whose output is thousands of lines;
- retries and a provider error mid-run;
- an approval pause;
- a compaction, which *replaces* history;
- a phase that turned out to be one step, and one that turned out to be forty.

**The material exists.** `~/.flux/events.db` holds real recorded runs; `flux export` (`run_export`,
`crates/flux-cli/src/export_cmd.rs`) renders a run's `run_trace`/`observations`/`plan_source` through
the same `Redactor` the live path uses. There is already a scenario-fixture precedent in the tree at
`crates/flux-sdk/tests/scenarios/coding-agent-note/model.jsonl`.

## ⚠ Committing a capture is a publishing act

The capture comes off a real machine into a **public repository**, and it is permanent once pushed.

⚠ `flux export` redacts what the `Redactor` **was told about** — the same limit
[C-432](C-432-browser-credentials-never-come-from-the-prompt.md) names, and
[C-339](C-339-redaction-falls-back-to-the-unredacted-value.md) is this repo's evidence that redaction
here has failed *open* before. A real coding run also carries things redaction was never aimed at:
absolute paths with a username, private source, internal hostnames, ticket and customer names.

**Treat the fixture as publishable content, not as test data.**

## Acceptance

- [ ] The fixture is derived from a **real recorded run**, and the exact capture command is recorded in
      the story so a future capture is reproducible in method even though the run is not.
- [ ] Which run was chosen, and **why it is representative**, is stated. ⚠ Do not pick the prettiest
      run — a run with a retry and an error in it is worth more here than a clean one.
- [ ] ⚠ **A human-reviewable redaction pass before commit**, and the diff is small enough to actually
      read. State what was scrubbed beyond what the `Redactor` did: usernames, absolute paths, hostnames,
      private source. ⚠ If the honest answer is "this run cannot be published safely", say so and capture
      a different one — do not soften it.
- [ ] The five mocks render the real run, and the same hard-case matrix (widths × cases) is regenerated.
- [ ] ⚠ **The recommendation is re-checked and explicitly confirmed or revised.** If the real run
      changes the answer, that is the single most valuable outcome this story can have — lead with it
      rather than burying it. If it confirms the answer, say which specific real-run property did the
      confirming, so the claim is anchored to something.
- [ ] The A-144 harness is reused. Only the fixture and whatever the real shapes force should change;
      a rewrite means the comparison is no longer against the same renderers.
- [ ] ⚠ Any real-run shape the mocks **cannot** represent is recorded rather than dropped — that is a
      finding about the layouts and about A-137's scope, not a gap to paper over.
- [ ] Full gate green.

## Notes

- Built on top of [A-144](A-144-five-tui-mocks-of-one-flow.md); branch from `impl/A-144`.
- ⚠ A committed capture is a snapshot and cannot be regenerated identically — state that where the
  fixture lives, so nobody later assumes it can be refreshed by re-running something.
- Keep a small synthetic case *alongside* if the real run turns out not to exercise a dimension (deep
  nesting is the likely one) — but label which is which. A fixture that silently mixes real and invented
  data is worse than either.
- ⚠ Related, same lesson from the other direction: [C-422](C-422-the-render-projection.md) found the
  TUI's durable→screen projection handles 5 observation kinds against 26 live variants. Whatever this
  story learns about reconstructing a real run from the log feeds directly into it.

## Progress

- Filed 2026-08-01 after the owner asked for a real example rather than a synthetic one.
