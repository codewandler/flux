---
title: Flux harness and tooling friction review — interactive pane exercise
date: 2026-07-31
kind: internal-review
lens: agent-efficiency-and-harness-ergonomics
method: >-
  Guided interactive exercise of pane discovery, open/update/close behavior, renderer changes,
  timed Flux-Lang updates, rerun and cleanup, followed by source-level comparison with the staged
  intent and agent-authored-surface designs. No benchmark, provider comparison, cancellation fault
  injection, or full test gate was run.
reviewer: agent
subject:
  repo: codewandler/flux
  surface: interactive TUI harness
  exercise: pane capability discovery and pane-animation-demo
verdict: >-
  The capability and authored-flow paths worked end to end. Intent-driven capability surfacing is a
  deliberate efficiency and safety feature, not a discoverability defect; the main opportunity is to
  make the intent declaration carry enough task structure and routing evidence to select the right
  narrow families reliably. Pane read-back remains a separate, already-tracked contract decision.
top_findings:
  - "The progressive intent-to-capability path is sound and should not be replaced by an all-tools catalog"
  - "Routing quality depends heavily on the intent contract and family hints; enriching that schema is the highest-leverage improvement"
  - "Pane mutation works well, including timed Flux-Lang updates and renderer changes"
  - "Pane state is intentionally not readable today; C-306 already owns that contract decision"
  - "A scoped transcript API could help retrospectives, but arbitrary access to session storage should remain forbidden"
---

## Verdict

The exercise was successful: the harness exposed the pane operations for an appropriate request, the
agent opened and updated live panes, a self-contained Flux-Lang flow performed timed updates and
changed renderer types, the flow reran, and the panes were closed. The authored flow is compact enough
to be useful as a real demonstration (`.flux/flows/pane_animation_demo.flux:4-37`).

The most important correction to the initial friction assessment is that Flux is not intended to show
every operation to every turn. The first model stage declares intent and selects the smallest relevant
capability families; only those families' exact live schemas are then exposed
(`docs/designs/staged-intent-native-planning.md:116-168`). This narrowing reduces context, prevents
irrelevant operation choice, and fails closed rather than widening to the whole catalog
(`docs/designs/staged-intent-native-planning.md:139-143`, `:271-286`). It is therefore a design
strength, not a capability-discoverability bug.

The efficiency question is narrower: can `declare_intent` describe the task and available routing
signals precisely enough that the correct small family set is selected on the first attempt? The
current worktree points in the right direction by giving the declaration explicit task kind, effect
mode, deliverable, constraints and uncertainties, while the family index can carry routing hints
(`crates/flux-flow/src/staged.rs:1576-1619`, `:1622-1672`). That is the highest-leverage seam to improve
and evaluate.

## What worked well

- **Intent-driven surfacing ultimately found the right capability.** The user moved from asking whether
  live pane control existed to explicitly asking for a pane demonstration. The relevant operations
  became usable without exposing an indiscriminate catalog. That is the intended staged behavior, not
  an accidental limitation.
- **The pane vocabulary is usefully constrained.** The model can choose a closed renderer kind,
  propose a slot and provide typed data, but cannot supply style, geometry, z-order or trusted chrome
  (`docs/designs/agent-authored-surface.md:48-67`, `:74-97`). The restriction still allowed progress,
  markdown, log, rows and key/value presentations in one demo.
- **Flux-Lang already supplies the timed-sequence primitive.** The demo uses authored `loop for 1s,
  every: 1s` blocks between pane updates and requires no shell command, process, or external I/O
  (`.flux/flows/pane_animation_demo.flux:1-35`). A new imperative `pane.sequence` operation is not
  necessary to prove or support this use case.
- **One flow made repetition cheap.** Once stored, the complete animation could be rerun as a single
  authored flow rather than reconstructed through ad hoc model calls.
- **Headless behavior is fail-closed by construction.** Pane operations are registered only when a
  surface sink is installed, rather than appearing in every headless catalog
  (`docs/designs/agent-authored-surface.md:113-133`; `docs/stories/C-223-pane-ops.md:38-47`). This is the
  right separation between operation availability and turn-level intent narrowing.

## Findings

### 1 — MEDIUM · Intent quality, not catalog breadth, is the capability-discovery bottleneck

The staged design deliberately gives the first provider call only `declare_intent`, then exposes exact
operation schemas only for accepted families (`docs/designs/staged-intent-native-planning.md:116-168`).
That architecture should remain. Showing all operations up front would spend context on irrelevant
schemas, weaken routing discipline, and undermine the fail-closed behavior.

The observed conversational path nevertheless required the user to sharpen the request before the
agent confidently described and exercised the pane surface. The useful improvement is therefore not a
`capabilities.list` escape hatch. It is a richer declaration contract and stronger family routing
hints so requests such as “can you update this harness UI?” map directly to the host-present surface
capability.

The current worktree already expands `declare_intent` beyond a free-form intent and family list with
`task_kind`, `effect_mode`, `deliverable`, `constraints`, and `uncertainties`
(`crates/flux-flow/src/staged.rs:1622-1672`). The family index also emits bounded operation examples and
optional routing hints (`crates/flux-flow/src/staged.rs:1576-1619`). This review treats that direction
as the intended fix, not as completed evidence: the files are modified in the reviewed worktree and no
routing evaluation was run here.

Recommendation:

- Keep capability families narrow and intent-driven.
- Add hermetic and live evaluation cases for indirect surface requests, direct pane requests, timed UI
  demos, transcript-history requests, and requests needing both workspace and surface capabilities.
- Measure first-pass family precision, unnecessary-family rate, repair rate, schema bytes surfaced and
  end-to-end latency.
- Prefer improving family descriptions/routing hints and the typed intent fields over adding an ambient
  all-capabilities discovery operation.

### 2 — LOW · The agent cannot verify authoritative pane state after sending commands

The pane contract is send-only. That means an operation can report that its command reached the sink,
but the agent cannot ask which panes remain after host-side expiry, suppression, `/resume`, capacity
limits, or user interaction. Reconstructing state from the agent's own calls would be wrong because the
host can drop state independently (`docs/designs/agent-authored-surface.md:135-144`).

This showed up in the exercise as an epistemic boundary: the agent could know what it submitted and the
user could confirm the visible result, but the agent could not independently inspect the current
rendered state. This is not an overlooked implementation task. C-306 explicitly owns the decision
whether the surface remains a write-only projection or gains an L2 query contract
(`docs/stories/C-306-pane-read-back-contract.md:15-48`).

Recommendation: resolve C-306 before adding `pane.list`, `pane.get`, rendered snapshots, or stronger
“visible” success claims. If read-back is accepted, query the host-owned store and test host-side expiry;
do not add tool-local shadow state. If write-only is retained, operation results and model-facing copy
should consistently say “command accepted” rather than imply that content is currently visible.

### 3 — LOW · Transcript retrospectives lack an authoritative scoped history reader

The agent can reason over conversation turns present in its current model context, but that does not
prove that the context is the complete persisted transcript after compaction, resume, or provider-side
omission. Operational evidence is not a substitute for message history, and arbitrary filesystem
access to user-global session storage would be the wrong remedy.

This matters for retrospective requests like this review more than for ordinary turn execution. The
review can accurately report the exercise represented in current context, but cannot independently
certify that no earlier relevant turn was omitted.

Recommendation: if full-session retrospectives are a supported product use case, expose a purpose-built,
policy-scoped and redacted conversation-history operation. It should define which roles and internal
events are visible, pagination, compaction markers, and whether the returned view is raw persisted
messages or a projection. Preserve workspace confinement; do not solve this by permitting arbitrary
reads of the session database.

### 4 — LOW · Timed pane flows are effective but verbose to author by hand

The eight one-second transitions work with ordinary Flux-Lang loops
(`.flux/flows/pane_animation_demo.flux:13-35`). This is architecturally preferable to shell-based sleep
and proves that no new process primitive is required. The cost is repetitive source: every frame needs
its own one-iteration loop and update call.

Recommendation: treat this first as a Flux-Lang ergonomics/documentation issue, not a new privileged
operation. A reusable composite operation or documented frame-loop pattern could reduce repetition
while preserving normal dispatch, approval and cancellation semantics. Add a native sequence primitive
only if profiling shows meaningful dispatch or jitter problems that authored composition cannot solve.

## Corrections to the initial friction list

The following earlier suggestions should not be carried forward as findings without qualification:

- **“Expose all capabilities or add `capabilities.list`.”** Rejected. The staged intent design
  intentionally narrows schemas and already provides a host-built family index. Improve routing rather
  than bypassing it.
- **“Pane operations were undiscoverable because they were absent initially.”** Reframed. Absence before
  accepted intent is expected. A failure to select the right family for a clear request would be a
  routing-quality issue, not proof that progressive surfacing is wrong.
- **“Add `pane.sequence`.”** Premature. The demonstrated Flux-Lang flow already performs a cancellable,
  self-contained timed sequence without shell I/O.
- **“Every pane frame needs separate approval.”** Not established by this review. The exercise did not
  collect a controlled approval trace, and authored-flow approval may aggregate effects. Benchmark the
  actual path before proposing a new approval model.
- **“Placement should always be authoritative.”** Rejected as stated. Slot is intentionally a proposal;
  the host may resolve, demote or suppress it to protect screen budget and trusted UI
  (`docs/designs/agent-authored-surface.md:85-91`, `:146-162`). Read-back, if accepted, may report the
  resolved state, but the agent should not gain geometry control.

## Suggested evaluation matrix

| Scenario | Expected family behavior | Success signal |
| --- | --- | --- |
| “Can you update this harness UI?” | Select the surface-owning family only | Pane schemas available on the next stage |
| “Open a progress pane and update it” | Select surface capability; no workspace/process family | Live pane command accepted |
| “Write and run a reusable pane demo flow” | Select workspace read/write plus surface as needed | File created, flow runs, pane changes |
| “What panes are currently visible?” | Honest unsupported result until C-306 is resolved | No invented state |
| “Read the full persisted conversation” | Select a future scoped history family if present | Complete/paginated projection with compaction markers |
| Pure explanation of pane concepts | No live capability required unless repository evidence is requested | Direct answer without catalog widening |

## Limitations and verification

- The review inspected the conventions in all three existing `docs/reviews/` documents and followed
  their frontmatter-plus-verdict/findings/limitations shape.
- Source evidence was read from the staged intent design and implementation, pane design and stories,
  and the authored demo flow.
- The live exercise is represented by the conversation supplied to this turn; no independent raw
  transcript reader was available to verify omitted history.
- No Cargo build, test, Clippy, rustfmt, provider comparison, latency benchmark, cancellation injection,
  narrow-terminal rendering check, resume/expiry check, or approval-trace analysis was run.
- The relevant implementation files already contain user-owned uncommitted changes. This review does
  not attribute those changes to itself or claim they have passed the repository gate.

## Bottom line

The harness demonstrated the intended architecture successfully: a narrow intent stage surfaced the
needed operation family, typed pane commands changed a trusted host-owned surface, and Flux-Lang
composed those commands into a timed reusable demo. The best efficiency investment is better intent
routing evidence and evaluation, not an all-tools catalog. The two genuine product questions exposed by
the exercise—authoritative pane read-back and authoritative full-session history—should be solved as
explicit scoped contracts, without weakening the guarded filesystem or trusted-surface boundaries.
