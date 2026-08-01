---
id: A-145
title: "Replace the mocks' hand-authored flow with a real recorded run — and re-check the recommendation against it"
pillar: Agent
status: done
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

- [x] The fixture is derived from a **real recorded run**, and the exact capture command is recorded in
      the story so a future capture is reproducible in method even though the run is not.
      → `crates/flux-tui/src/loopmock/captures/s_1477-docs-and-release.jsonl`, produced by
      `crates/flux-tui/examples/capture_run.rs`; the command is the capture's own header line.
- [x] Which run was chosen, and **why it is representative**, is stated. ⚠ Do not pick the prettiest
      run — a run with a retry and an error in it is worth more here than a clean one.
      → see "The run" below.
- [x] ⚠ **A human-reviewable redaction pass before commit**, and the diff is small enough to actually
      read. State what was scrubbed beyond what the `Redactor` did: usernames, absolute paths, hostnames,
      private source. ⚠ If the honest answer is "this run cannot be published safely", say so and capture
      a different one — do not soften it.
      → see "The redaction pass" below. 713 lines, every free-text field read.
- [x] The five mocks render the real run, and the same hard-case matrix (widths × cases) is regenerated.
      → `docs/designs/agent-loop-visibility-mocks.md`, 50 renders. ⚠ Two of the four cases are real;
      the other two could not be (see below), and every header says which.
- [x] ⚠ **The recommendation is re-checked and explicitly confirmed or revised.** If the real run
      changes the answer, that is the single most valuable outcome this story can have — lead with it
      rather than burying it. If it confirms the answer, say which specific real-run property did the
      confirming, so the claim is anchored to something.
      → CONFIRMED with three corrections, led with, at the top of `loopmock::RECOMMENDATION` and in
      the design doc. The confirming property is named: the one-step phase beside the 57-step one.
- [x] The A-144 harness is reused. Only the fixture and whatever the real shapes force should change;
      a rewrite means the comparison is no longer against the same renderers.
      → the five renderers, `Tally`, `clip`, `window`, the floors and the property sweep are
      untouched. The forced changes are listed under "What the real shapes forced" below.
- [x] ⚠ Any real-run shape the mocks **cannot** represent is recorded rather than dropped — that is a
      finding about the layouts and about A-137's scope, not a gap to paper over.
      → `loopmock::capture::FIDELITY`, rendered into the snapshot set as a committed table.
- [x] Full gate green.

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

## The run

**`s_1477`** — gpt-5.5, nine turns, 33 minutes, 191 reconstructed steps. The operator asks what is
special about this SDK, then has it audit whether the docs kept up with the day's commits, fill the
gaps, commit them, and cut a release.

Why this one, out of 1 465 sessions in the store. It was scored against the shapes the story names,
not against how it reads:

| shape the story asked for | in `s_1477` |
|---|---|
| an approval pause | **four**, and they are real operator pauses: 71.8 s, 39.7 s, 7.9 s, 5.8 s |
| a failure | a `git_stage` that failed on a pathspec, plus one flow-level `step_failed` |
| retries | 57 `model.call` rows with `repair_attempt > 0` |
| wildly uneven durations | 2 ms → 344 s in one session; five orders of magnitude |
| a very long tool output | 44.9 KB from one `read_many`; the biggest single op output is 175 KB |
| a phase of one step and a phase of forty | 36 phases of exactly one step, and one of **57** |
| publishable | every path is repo-relative, every file is this repo's own public source |

⚠ **What it does not have, and what that cost.** No sub-agents, no compaction, and no provider
error. The first two are not properties of this run but of the **store**: not one of the 112 114
events is a `Compacted`, and no session that records op output (post-C-43) also spawns sub-agents —
all twelve sub-agent parents predate `OpRecorded`. So the fan-out and deep-nesting cases could not
be made real by picking a different run, and they stay hand-authored and labelled.

**The runs deliberately rejected.** `s_1113` and `s_1107` are the richest sessions in the store
(GitLab release coordination, Slack triage) and are **not publishable at any effort**: internal
project paths, a customer name, Slack channel and user ids. `s_1591` and `s_1486` carry
`/home/<user>` paths in op output. Stated rather than quietly skipped, because "I picked the one
that was safe" is the answer, and the reason the others were not is the story's whole point.

### The capture command

```sh
cargo run -p flux-tui --example capture_run -- \
    --session s_1477 --title "docs gap audit, fix, commit, release" \
    > crates/flux-tui/src/loopmock/captures/s_1477-docs-and-release.jsonl
```

⚠ **This does not regenerate the fixture.** A capture is a snapshot of one machine at one moment;
re-running it against a different session records a different run. The *method* is reproducible, the
run is not.

## The redaction pass

`flux export` runs `run_trace`/`observations`/`plan_source` through the `Redactor`, which only knows
the values it was **told about**. That is the C-432 limit, and C-339 is this repo's evidence that it
has failed open before. So the capture does not filter — it **allow-lists**:

1. **An exhaustive `match` on `EventKind`.** Only `SessionStarted`, `TurnStarted`/`TurnEnded`,
   accepted `PlanAttempted`, `CallUsage` and six `RunEvent` variants are emitted; each field is
   named individually. A new durable fact will not compile until somebody decides about it.
2. **Every `Observation` is dropped.** This alone removes the largest leak surface in the session:
   `turn.identity` and every `tool_call` carry a `caller` — **192 rows of the local username** — and
   the `toolchain` observation lists every locally installed plugin operation, which names the
   downstream consumer 800 times. None of it is anything the loop view draws.
3. **Every free-text field is cut to 100 characters of its first line** before anything else, so a
   long tool output cannot smuggle content past a reviewer in its tail.
4. **A boundary-aware deny-list scrub** over what survives: absolute home paths (`/home/`,
   `/Users/` → `/<home>/`), downstream consumer names, credential prefixes (`sk-`, `ghp_`, `xoxb-`,
   `AKIA`, `Bearer `, PEM headers) and internal addresses (`127.0.0.1`, `192.168.`, `.internal`).
   Substitutions are visible words, not `***`, so a reader can tell a scrub from a value. Word-start
   matching is deliberate: `sk-` is a substring of "risk-gated", and a scrub that mangles prose is a
   scrub nobody trusts.
5. **A re-scan of the finished artifact** — the command fails rather than emits a capture that still
   matches one of its own patterns.

**On this run the scrub matched nothing**: steps 1–3 had already removed everything it looks for.
That is the honest result and not a claim that the scrub is unnecessary — it is the net.

**Then it was read.** 713 lines, ~92 KB, and every distinct value of every free-text field was
enumerated and inspected by hand: 124 tool inputs, 102 tool outputs, 126 plan sources, 9 prompts,
9 answers, 1 error. What is in there is this repository's own public content — `crates/flux-sdk/`,
`docs/designs/`, `website/docs/`, `CHANGELOG.md` — at repo-relative paths. The only URL is
`https://github.com/codewandler/flux`, from the SDK README.

⚠ **Nothing was scrubbed beyond what is listed above, because nothing needed it.** Had this run
required per-value redaction to be publishable, the right answer would have been to capture a
different one — that is what happened to the four runs named above.

## What the real shapes forced

The harness is reused: the five renderers, `Tally`, `clip`, `window`, the floors and the property
sweep are untouched. Four things had to change, each because the real data would not fit otherwise,
and each is itself a finding:

1. **`Provenance` on `Fixture`, drawn in every header.** Two of four cases are real. A comparison
   that silently mixes measured and invented load is worse than either, so which is which is on
   screen rather than in a doc comment.
2. **A replay cursor.** The log has no present tense — every recorded step is finished — so a
   replayed session has no running step, and "what is it doing right now" is the only question a
   live loop view exists to answer. The reconstruction replays to one instant, deterministically
   chosen, and says so in the header.
3. **The graph mock's gutter is derived, not declared.** A hand-authored fixture can be handed a
   nine-node `plan_ast` and a matching table. A recorded one cannot: this loop emits **one accepted
   plan per op** and never persists `plan_ast` at all.
4. **`Status::Pending` left the recorded cases.** The log has no future; an adaptive run authors no
   plan beyond its next op. The timeline's pending-step test moved to a hand-authored case, and the
   move is the finding.

## Progress

- Filed 2026-08-01 after the owner asked for a real example rather than a synthetic one.
- 2026-08-02 — implemented. `loopmock::capture` is the reconstruction and `FIDELITY` its table;
  `examples/capture_run.rs` is the capture command; the capture is committed. Tidy and long-run are
  reconstructed from `s_1477`; deep nesting and fan-out stay hand-authored because the store cannot
  produce them. The recommendation is **confirmed with three corrections**, led with rather than
  buried — see the design doc and the top of `loopmock::RECOMMENDATION`.
- ⚠ Open for whoever picks up A-137: the cursor rule ("halfway through the last recorded step")
  decides what the split's pane has to show, and at the end of a run that is a 2 ms `present_results`
  with nothing in it. The pane's value claim is better evidenced by the tidy case than the long-run
  one. A different rule would flatter it; that is why this one is mechanical.
