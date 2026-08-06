# Fleet measurement: the system, the workers, and the assignments

Epic: score-the-fleet-per-agent-with-service-levels. That design defines *scoring and enforcement*
(profiles, SLOs, error budgets, the scoreboard); this one defines *what is measured and why* — the
metric catalogue, its evidence base, the telemetry it needs, and who consumes it. C-573 (bounded
policy controller) is the third leg: it acts on these numbers inside an operator-authorized policy.

## Why

Every judgement made about the fleet so far has been anecdotal, and several were wrong in ways a
measurement would have caught immediately. The 2026-08-05/06 dogfood run over the family roadmap
workspace produced the receipts:

- 30 waves and 50 admitted agents delivered **one** applied story (~3% wave yield). One story was
  dispatched 13 times, another 12. Nothing surfaced the waste while it accumulated.
- Turn status lied in both directions: a turn recorded `failed` while holding a complete clean
  commit; turns recorded `success` while holding nothing. Any yield number derived from turn status
  is fiction — only git and gate evidence count.
- Workers spent 64–75% of their calls on read/grep/glob; one spent 30 of 60 rounds on sequential
  single reads; the staged-batch machinery cost ~3 non-productive events per effect. None of this
  was visible until a human read transcripts by hand.
- Twelve workers each burned a round on a guaranteed-denial tool call; several burned rounds
  discovering facts the host already knew (base commit, branch, crate naming). This is assignment
  quality, not worker quality — and nothing distinguishes the two today.
- A ~12h coordinator session logged 41 human interventions and ~3.5h of idle waiting. The human is
  a system component with no latency metric.
- Harness changes (checkpoint loop, model swap, fresh-context workers) were real interventions
  judged by whether the next wave *felt* better. No before/after number exists for any of them.

Three distinct things need measuring, and conflating them is itself a failure mode:

1. **The system** — is the pipeline worth running, and where does it lose time, tokens, disk and
   human attention?
2. **The workers** — per profile `(role, model, loop revision)`: how efficiently does a bounded
   turn convert budget into verified delivery?
3. **The assignments** — was the story, its context and its instructions good enough that a
   competent worker could execute without guessing? This dimension exists nowhere today, and it is
   where several of the worst dogfood losses actually belonged.

## Sources and research

### Internal evidence (the dogfood corpus)

The family roadmap workspace's fleet event journal, activity projection, driver log, worker
transcripts and coordinator session for 2026-08-05/06, mined post-hoc; findings are contracted in
stories C-594–C-634 and the roadmap workspace's R-10–R-26. Headline numbers used throughout this
design: 3% wave yield; 13×/12× duplicate dispatch; 64–75% exploration share; ~3:1
overhead-per-effect; 16 guaranteed policy denials; 0% report fidelity (a loop return bug destroyed
every worker self-report); 41 interventions per delivered story.

### External research

- **Cost-of-pass** — expected cost of one *correct* solution (`mean cost per attempt ÷ pass
  rate`), the standard efficiency currency in agent evaluation; framework design alone moved it
  28.4% at ~97% retained performance. *Efficient Agents: Building Effective Agents While Reducing
  Cost*, arXiv:2508.02694. <https://arxiv.org/abs/2508.02694>
- **pass^k** — probability that all k i.i.d. attempts succeed (`p^k`); a 90% pass@1 agent is ~57%
  at k=8. The honest reliability number for anything that runs unsupervised. *τ-bench*,
  arXiv:2406.12045. <https://arxiv.org/abs/2406.12045>
- **Coordination overhead, error amplification, token efficiency** — `O% = (T_MAS − T_SAS)/T_SAS`
  measured at 58–515% across architectures; error amplification `E_MAS/E_SAS` of 17.2× for
  independent agents vs 4.4× with centralized verification (hard evidence for reviewer loops);
  token efficiency as successes per 1k tokens; inter-agent message density saturates (~0.39
  messages/turn). *Towards a Science of Scaling Agent Systems*, arXiv:2512.08296.
  <https://arxiv.org/html/2512.08296v1>
- **Multi-agent cost compounding** — agents cost ~4× chat, multi-agent ~15×; justified only when
  the capability delta exceeds the multiple. <https://www.augmentcode.com/guides/multi-agent-cost-compounding>
- **Early termination** — experience-driven abort of doomed trajectories as a first-class cost
  lever. *EET: Experience-Driven Early Termination*, arXiv:2601.05777.
  <https://arxiv.org/pdf/2601.05777>
- **Trace-based evaluation** — step efficiency, tool correctness, plan adherence scored over
  recorded trajectories; the industry frame for what the roadmap retro pipeline does.
  <https://www.confident-ai.com/blog/llm-agent-evaluation-complete-guide>
- **Durability over volume** — AI-era delivery findings: individual output up (~2× merged PRs)
  while organizational delivery stays flat; churn climbing; rework rate as the hidden tax; read
  volume against durability, never alone. <https://getdx.com/blog/dora-metrics/>,
  <https://zylos.ai/research/2026-02-07-developer-productivity-metrics/>
- **Agent SRE** — SLOs with error budgets for agents, burn-rate alerting (act at ~5× sustainable
  burn, before breach), pre-action budget gates, graduated exhaustion actions
  (alert/throttle/freeze/circuit-break).
  <https://microsoft.github.io/agent-governance-toolkit/packages/agent-sre/>,
  <https://techcommunity.microsoft.com/blog/linuxandopensourceblog/applying-site-reliability-engineering-to-autonomous-ai-agents/4521357>
- **OpenTelemetry GenAI semantic conventions** — the emerging vendor-neutral schema for agent
  telemetry: `gen_ai.usage.input_tokens`/`output_tokens`, `invoke_agent`/`execute_tool` span
  shapes, token-usage and tool-duration histograms; natively consumed by major observability
  backends. <https://opentelemetry.io/blog/2026/genai-observability/>,
  <https://www.datadoghq.com/blog/llm-otel-semantic-convention/>

## Approach

### Principles (each one paid for)

1. **Host-derived facts only.** No metric is computed from an agent's self-report; report fidelity
   is itself a metric, so grading on self-reports would let the measured system grade itself.
2. **Denominators come from git and gates.** Turn status is excluded from every yield number.
3. **Marginal and fully-loaded costs are separate.** Cost-of-pass is fully-loaded (all attempts,
   including failed and duplicate waves); the winning attempt's cost is reported beside it. The
   ratio between them is the waste multiplier, and it is the single best system-health number.
4. **Every efficiency metric is paired with a quality gate.** Commits-per-token alone incentivizes
   micro-commits; it is only reported next to first-pass gate rate and durability. Goodhart is a
   design constraint, not a footnote.
5. **Insufficient evidence is a first-class state** (from the epic): windowed minimums, hysteresis,
   and sample counts displayed with every number. Agent populations are tiny; a single failure is
   not a signal.
6. **Every value carries source, freshness and reported/estimated/unsupported state** (aligned
   with C-573's projection contract). Unknown cost is never zero.
7. **A cadence is verified over at least three intervals** before any fix or regression claim.

### Target 1 — the system

Scope: the workspace over a trailing window. The honest denominator is stories merged to a
member's `main`, not handoffs and not commits.

| Metric | Definition | Source of truth | Available |
|---|---|---|---|
| Delivery yield | stories merged / waves dispatched | journal + git | now |
| Waste multiplier | attempts per delivered story (all waves that touched it) | journal | now |
| Cost-of-pass | fully-loaded tokens (and priced cost) / delivered story | usage telemetry | needs C-632 |
| Stage dwell | time in ready→dispatched→committed→handoff→integrated→applied→done | journal + board | now (coarse) |
| Park economics | parks/wave, park dwell, % parks resolved without a human | driver state | now |
| Human tax | interventions per delivered story; attention latency (raised→answered) | decisions journal | now (coarse) |
| Gate economics | gate runs per candidate (must be 1.0); gate minutes + GB per delivery | journal + driver | now |
| Duplicate work | dispatches refused by delivered-filter; duplicate waves created | journal | now |
| Driver liveness | ticks between halts; consecutive idle ticks | driver log/state | now |
| Post-merge durability | 14-day churn and revert rate on fleet-delivered lines; duplication share | git blame at +14d | now |
| Coordination overhead | `O% = (T_fleet − T_single)/T_single` vs an occasional single-agent calibration story | usage + calibration runs | needs C-632 |
| Error amplification | duplicate/conflicting work events per story, fleet vs single baseline | journal | now (proxy) |

### Target 2 — the workers

Scope: the profile `(role, model, loop revision)` — never the ephemeral instance (per the epic).
The epic's verified-outcome SLIs (delivery, first-pass gate, rework, report fidelity, evidence
quality, contract adherence, cost, latency) stand; this catalogue adds the **budget anatomy** —
where a turn's rounds actually went:

| Metric | Definition | Source of truth | Available |
|---|---|---|---|
| pass^k reliability | probability all k attempts on story-shaped work succeed, per profile | journal (repeat attempts exist) | now |
| Token efficiency | verified deliveries per 1k tokens | usage telemetry | needs C-632 |
| Exploration ratio | read/grep/glob calls ÷ total calls | activity/receipts | now |
| Time-to-first-effect | calls (and wall time) before the first mutation; also time-to-first-commit | activity/receipts | now |
| Overhead per effect | plan+approval+staging events ÷ effectful calls | activity | now |
| Guillotine proximity | rounds used ÷ limit at terminal state; hard counter: turns killed by a ceiling while holding uncommitted work | usage telemetry | needs C-632 |
| Dead-call rate | guaranteed-failure calls: policy denials, out-of-fence probes, wrong package names | activity/receipts | now |
| Batching quality | batched reads ÷ batchable read sequences | receipts | now |
| Zero-yield turns | turns with no edit and no commit, split refused-honestly vs burned-silently | receipts + git | now |
| Report integrity | worker-reported SHA/write-set agreement with git ground truth | handoff + git | now (canary: currently 0% for a known bug) |
| Salvage rate | tokens saved by early termination vs tokens lost to ceilings (once checkpoint can abort) | usage + journal | later |

### Target 3 — the assignments

Scope: the story, its epic, and the instruction/loop revision that framed the work. These metrics
attribute failure to the *input*, not the agent — the dimension nothing measures today. The
objective half comes from host-observable behavior; the subjective half comes from structured
reflections (C-587; interim: the roadmap retro records).

| Metric | Definition | Source of truth | Available |
|---|---|---|---|
| Guess rate | calls spent discovering facts the host knew (base commit, branch, crate names, network state) | activity + assignment manifest | now |
| Question-instead-of-work | turns ending in a clarifying question instead of a commit, per story and per instruction revision | receipts | now |
| Scope accuracy | declared story scope (areas/crates) vs actual write set, both directions | story + git | now |
| Context sufficiency | distinct files read before first effect vs files in the final write set | receipts + git | now |
| Contract contradiction | instructions that conflict with a gate or fence (any occurrence is a sev-1 planning finding) | retro synthesis | needs retro |
| Acceptance verifiability | share of acceptance boxes that map to a runnable check | story parsing | now |
| Prompt-fact errors | assignment facts contradicted by ground truth (stale deps, wrong names) | retro synthesis | needs retro |
| Reflection signals | C-587 fields: context quality, missing information, missing/awkward tools, ambiguous instructions, budget pressure | reflections | needs C-587 (interim: retro records) |
| Revision cohorts | same-shaped stories under instruction/loop revision N vs N+1, compared on exploration ratio, rounds-to-commit, first-pass rate | all of the above, keyed by admitted digest | needs C-627 |

### Telemetry substrate

What the catalogue needs, in dependency order — these are existing stories, not new ones:

- **C-632** — usage (rounds consumed/limit, input/output tokens) on every terminal turn event,
  including failures; ordered, timestamped, tear-proof activity records. Field names align with
  the OpenTelemetry GenAI semantic conventions (`gen_ai.usage.input_tokens`, …) so fleet telemetry
  is readable by standard backends without translation.
- **C-633** — worker session stores survive their turns; failed turns are the highest-signal
  input and currently leave almost nothing.
- **C-627** — revision↔digest integrity, which is what makes cohort comparison trustworthy.
- **C-602** — the live activity channel, for streaming rather than post-hoc collection.
- Roadmap retro pipeline (R-22/R-23) — the interim producer of reflection-shaped records until
  C-572/C-587 land the native reviewer/reflection mechanism.

The derived numbers live in a **bounded, incrementally maintained metrics projection** (the
epic's constraint, learned from a 2.7 MB status projection): outcomes are attributed when
verified, to the profile and assignment recorded at spawn, and the projection is append-updated —
never re-derived from the journal on read. Every row: value, window, sample count, source,
freshness, reported/estimated/unsupported.

### Consumers

- **The service-level scoreboard** (the epic): profiles scored on the verified SLIs; budget
  consumption drives the graduated consequences. This design adds pass^k as the reliability SLI,
  **burn-rate alerting** (act when the budget burns at ~5× sustainable rate, before breach), a
  **pre-dispatch budget gate** (a profile with an exhausted budget stops receiving auto-selection
  — the epic's consequence, framed as the SRE gate), and **post-merge durability** as a
  quality-side SLI so volume is always read against survival.
- **C-573's policy controller**: cost-of-pass is the headline objective its bounded actuators
  optimize; the single-agent calibration baseline and coordination-overhead trend are recorded
  inputs for the "is this width/model worth it" decisions.
- **The roadmap replan tick**: durability, cost-of-pass and waste-multiplier trends as standing
  inputs to promotion and lane proposals.
- **The retro synthesis**: every routed finding names the metric it expects to move, and the
  finding is closed by the number moving — the self-improvement loop's keep-only-if-it-helps
  invariant, applied to process changes as well as code.

### Staging and story boundaries

Stories are cut per stage when a stage starts; this design deliberately files none now.

1. **Stage 0 — derive from what exists.** An offline projection over the journal, git and the
   driver state: yield, waste multiplier, stage dwell, park/gate economics, exploration ratio,
   dead calls, zero-yield turns, pass^k from historical repeat attempts, durability at +14d.
   No harness change; proves the catalogue on real data and bootstraps baselines.
2. **Stage 1 — telemetry lands** (C-632, C-633, C-627 in that order): budget anatomy and exact
   cost-of-pass become computable; OTel-aligned fields appear on turn events.
3. **Stage 2 — scoring and consequences** (the epic's stories) plus C-573's controller reading
   the projection.
4. **Stage 3 — assignment quality**: retro/reflection-fed metrics and revision cohorts; the
   planner's promotion gate may then cite acceptance-verifiability floors.

### Non-goals

- No auto-tuning outside C-573's operator-authorized policy; this design produces numbers, not
  decisions.
- No per-metric stories yet, and no dashboard beyond the epic's scoreboard pane.
- No scoring of individual humans. The human tax and attention latency measure the *system's*
  demand on its operator, not the operator.
