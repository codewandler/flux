# Design — Score the fleet per agent with service levels

Measurement foundation: `docs/designs/D-255-fleet-measurement-the-system-the-workers-and-the-assignments.md`
defines the metric catalogue this epic scores against — three measurement targets (system, worker
profiles, assignments), the evidence base and external research behind each metric, and the
telemetry substrate (C-632/C-633/C-627/C-602) the numbers require. This design defines scoring,
objectives and enforcement over that catalogue.

## Why

The fleet has no notion of how well it is working. Every judgement made about it so far has been
anecdotal, and several of those judgements were wrong in ways a measurement would have caught
immediately:

- A worker's turn status is not its delivery. A turn recorded `failed` while holding a complete,
  clean commit, and a turn recorded `success` while holding nothing. Any impression of "the fleet is
  working" taken from turn status is unfounded.
- A worker's own handoff is not verified truth. Handoff evidence is re-run by the host at the pinned
  base and at the commit precisely because the self-report can be wrong — but the *rate* at which it
  is wrong is never recorded, so there is no basis for trusting or distrusting a role.
- Harness changes are made and then judged by whether the next wave felt better. An authored
  checkpoint loop, a fresh-context worker, a model change: each was a real intervention with no
  before/after number attached.
- Width, model choice and loop version are configured by argument rather than by outcome. There is
  currently no way to answer "is opus worth it for this role" or "did the checkpoint loop raise
  first-pass rate" with anything but an impression.

Service-level thinking is the right frame because the fleet is a service that other work depends on,
and because the SLI/SLO/error-budget shape already solves the specific problem here: turning noisy
per-instance outcomes into a windowed statement that can carry consequences.

One thing must be said plainly, because it is where this analogy breaks: an agent is not a web server.
Its outcomes are high-variance and its population is tiny — a role may complete four stories in a day.
A single failure is not a signal. Every target here is therefore windowed with a minimum sample count,
and every enforcement has hysteresis. Without that, scoring produces confident nonsense and flapping
policy, which is worse than no scoring at all.

## Approach

### What is scored

Not an agent instance — those are ephemeral and each completes one story. The scored unit is a
**profile**: `(role, model, loop version)`. That is the unit an operator can actually change, and it is
what makes a harness change measurable: the same role under a new loop is a new profile, so before and
after are directly comparable instead of blended.

Individual agents still carry their own record, for forensics. They are not scored.

### Indicators (SLIs)

All derived from evidence the fleet already produces. Each is defined in terms of *verified* outcomes,
never self-reported ones.

| Indicator | Definition | Why this one |
|---|---|---|
| Delivery | assignments that produced a host-verified accepted handoff | The only honest measure of "did it work"; turn status is not it |
| First-pass gate | candidates green on their first integration attempt | Distinguishes "produced a commit" from "produced a correct commit" |
| Rework | rework rounds consumed per delivered story | The bounded resource the supervisor already tracks |
| Report fidelity | agreement between the worker's own handoff and host verification | Measures whether the agent's word can be taken; the sharpest indicator here |
| Evidence quality | share of handoffs whose cited test genuinely failed at base and passed at commit, `ran_no_tests` counted as a miss | A filtered-to-nothing run is neither pass nor failure, and used to read as green |
| Contract adherence | refused operations, fenced-path attempts, writes outside the declared write set | Hard-bounded, not a percentage: any violation is a finding |
| Cost | tokens, wall clock and build time per delivered story | Makes a model or loop change comparable on price, not only on quality |
| Latency | dispatch to accepted handoff, plus time held in each state | Exposes waiting that is invisible in aggregate throughput |

### Objectives (SLOs) and error budget

An objective is a target on one indicator over a trailing window, with a minimum sample count below
which the objective reports *insufficient evidence* rather than pass or fail. Insufficient evidence is
a first-class state and must be displayed as itself — reporting an unmeasured profile as healthy is the
failure mode this design exists to prevent.

The error budget is the ordinary one: an objective of 80% over 20 samples permits 4 failures, and what
matters operationally is how much of that allowance is left. Budget consumption is the number the
supervisor acts on, because it is directionally meaningful before the objective is breached.

### The agreement (SLA) — service levels with consequences

An objective nobody acts on is decoration. Here the counterparty is the supervisor, so the consequences
are automated and graduated:

1. **Budget healthy** — the profile keeps its concurrency share and stays eligible for auto-selection.
2. **Budget mostly consumed** — the profile's candidates require review before integration.
3. **Budget exhausted** — the profile loses auto-selection: it runs only when an operator names it.
4. **Hard violation** (contract adherence) — the wave stops and escalates immediately, without a
   window. A fence breach is not a rate.

Each transition needs hysteresis and a stated dwell time, or a profile oscillates between states on
one story's outcome. Every transition is journalled with the samples that caused it, so a demotion can
always be audited — and, importantly, appealed by inspecting the evidence rather than by argument.

### Where the numbers come from

A bounded, incrementally maintained projection — not a re-derivation from the event journal. This
constraint is not theoretical: a status projection that embedded historical turn events reached 2.7 MB,
and rendering fleet projections on the event loop has already frozen the surface once. The scoreboard
must read a small precomputed summary, and the summary must be updated as outcomes land.

Attribution must survive restarts and reclamation, so an outcome is attributed when it is verified, to
the profile recorded at spawn — not recomputed later from configuration that may have changed.

## UI / TUI

**A scoreboard pane, one row per profile.** Role, model, loop version, then one column per objective:
value, target, and a state glyph — `ok` / `at risk` / `breached` / `no data`. Sortable by budget
consumed, because "closest to trouble" is the useful default ordering, not alphabetical.

**Error budget as a bar, not a percentage.** A bar shows consumed against remaining at a glance, with
the sample count beside it so a 100%-on-2-samples row cannot masquerade as a strong result.

**Sparkline per indicator over the window.** Direction matters more than level: a profile at 75%
climbing is a different situation from one at 75% falling, and a single number hides that.

**Expanding a row shows the samples.** Every story that contributed, with its verified outcome and a
link to its evidence — the same expand-to-detail behaviour the board pane is gaining. A score whose
inputs cannot be inspected will not be trusted, and should not be.

**Compare mode.** Two profiles side by side with their deltas, so "opus versus sonnet for this role" or
"checkpoint loop versus adaptive" is a screen rather than an analysis.

**State is carried by glyph and label, never by colour alone.** Colour is an accent; this surface must
stay readable when colour is unavailable, and one pane already lost its colours entirely to an
inherited `NO_COLOR`.

**Read-only, and structurally incapable of mutating planning state.** The pane displays a projection.
It has no path to a board or fleet mutation, which is a property to test, not a convention to observe.

**Truncation is visible.** Where the pane bounds what it shows, it says what it dropped. A silently
truncated scoreboard reads as complete coverage.

## Stories

- Define the scored profile `(role, model, loop version)` and attribute every verified outcome to the
  profile recorded at the agent's spawn.
- Compute the indicators from verified evidence only, with `ran_no_tests` counted as an evidence miss
  and turn status explicitly excluded.
- Record report fidelity: the divergence between a worker's own handoff and host verification.
- Maintain a bounded, incrementally updated score projection; the surface never re-derives it from the
  journal.
- Express objectives as windowed targets with a minimum sample count, and report insufficient evidence
  as its own state.
- Graduate supervisor consequences on error-budget consumption, with hysteresis, and journal every
  transition with the samples that caused it.
- Escalate a contract-adherence violation immediately, without a window.
- A TUI scoreboard pane: one row per profile, budget bars with sample counts, sparklines, and glyph
  state independent of colour.
- Expanding a scoreboard row lists the contributing stories and their evidence.
- Compare two profiles side by side with deltas.
- The scoreboard pane is read-only and cannot reach a board or fleet mutation.
