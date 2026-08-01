# Design: What the Pi comparison says to fix — and what to defend instead

**Status:** proposed · **Pillar:** Core · **Stories:** [C-444](../stories/C-444-sdk-secure-defaults.md) · [C-445](../stories/C-445-interactive-confinement-posture.md) · [C-446](../stories/C-446-no-windows-sandbox-backend.md) · [C-447](../stories/C-447-the-per-engine-turn-mutex.md) · [C-448](../stories/C-448-cancellation-coverage.md) · [C-449](../stories/C-449-provider-breadth.md) · [C-450](../stories/C-450-dependency-pinning-discipline.md) · [C-451](../stories/C-451-the-head-to-head-benchmark.md) · [C-452](../stories/C-452-what-flux-defends.md)

## Why

`docs/reviews/single/2026-08-01-pi-flux-harness-comparison.md` is two isolated source-level reviews
against one nine-axis rubric, with primary-evidence cross-check. It is the most rigorous outside read
flux has, and it names specific, line-cited gaps.

## ⚠ First, the framing — "become better in the review" is a trap if taken literally

Several axes flux scores lower on are **things flux chose**, and closing them would make flux worse:

- **Performance / complexity — 6.5 vs 7.5.** The stated reason is 38 crates and a mandatory envelope.
  Removing the envelope raises the score and destroys the product.
- **Extensions / maximum freedom — Pi wins on "in-process TypeScript can replace nearly every layer."**
  The review's own next sentence: *"This is also why it is not a security boundary."*
- **Ecosystem — 7.0 vs 8.0.** 81,617 stars against zero. **Not closable by code**, and the review says
  so: it *"lowers integration discovery risk, not execution risk."*

So the epic sorts every finding into three buckets, and the third is not a lesser outcome:

| bucket | what it means |
|---|---|
| **close it** | a real gap; flux's own claims are weaker than they should be |
| **defend it** | a deliberate trade-off; the fix is *saying so*, not changing it |
| **not code** | adoption, maintainer depth, production history |

⚠ The review's most useful property is that it distinguishes these already. **Reading it as a scorecard
would invert its actual advice** — its Bottom Line is *"choose Flux when the runtime must remain the
authority after the model, prompt and workflow have spoken."*

## The real gaps, in the order they matter

### The one that undercuts flux's headline claim — C-444

flux's argument is that authorization and approval are **runtime types that cannot be disabled**. Two
findings show an embedder can fall out of that without noticing:

- *"The SDK also states that `auto_approve(true)` does not imply confinement; the embedder must set
  it"* (F2, `crates/flux-sdk/src/lib.rs:17`).
- *"The SDK's runtime-use ceilings are unbounded by default and per agent; a delegated tree can
  multiply its concurrent tool count"* (F4, `crates/flux-sdk/src/lib.rs:792`).

⚠ Both are documented, and documented is not defaulted. An embedder who reads the headline and not the
caveat gets auto-approval with no confinement and no ceiling — which is precisely the configuration the
review calls a poor fit. This is where "asks more of the embedder" (Embeddability 8.0 vs 9.0) becomes a
safety statement rather than an ergonomics one.

### The remaining secure-defaults gap — C-445

The review is precise about what is left: *"unattended CLI execution is fail-closed by default, but
interactive and SDK usage still require an explicit OS isolation decision"* — and *"even installed
plugin startup can run unconfined"* (`crates/flux-cli/src/dispatch.rs:111`). C-410 closed the
unattended surfaces; interactive is deliberately exempt. **Deliberate is a decision that should be
re-taken with the finding in hand**, not inherited.

### Windows — C-446

*"Windows has no native backend in the reviewed tree."* So *"Flux has a mandatory policy/guarded-IO
boundary everywhere, but not a mandatory OS isolation boundary everywhere."* Either build it, or say
plainly what a Windows deployment does and does not get.

### The turn mutex — C-447

*"All public turn entries acquire the same `turn_gate` mutex"* (`crates/flux-flow/src/engine.rs:713`) —
a strong identity/session-integrity simplification **and** a real throughput ceiling. The review turns
it into an open question worth answering on its own terms: *can it be removed without weakening
immutable turn identity or session validity, and what real throughput target requires it?*

### Where Pi is genuinely ahead on capability

- **Cancellation (C-448).** Pi's *"reaches the high-level agent and session-owned work, including
  retries, compaction and bash."* Whether flux's does is unaudited.
- **Provider breadth (C-449).** *"39 provider variants in the pinned tree."* Both scored 9.0 on the
  providers/context/sessions axis, so this is product surface rather than a defect — but breadth is a
  real adoption lever.
- **Dependency discipline (C-450).** Pi *"mechanically pinned"* direct dependencies and requires
  explicit review of new lifecycle scripts. Different ecosystem, transferable idea — and flux already
  has the harder half (attested releases, commit-pinned Actions).

### The question that would settle the rest — C-451

The review's first open question: *"How do the two harnesses compare on the same model, repository,
task corpus and approval posture for success rate, latency, token cost and operator interventions?"*
⚠ **No task-quality or runtime benchmark was performed.** Every performance claim in the comparison —
in both directions — is an inference from source. flux has `flux-eval` and a harness for exactly this.

### And the one that is documentation — C-452

Write down what flux **defends rather than closes**, with the reasoning. Otherwise the next review, and
the next reader, re-derives it — and worse, someone eventually "fixes" the envelope to win a rubric.

## Alternatives considered

- **File one remediation story per rubric axis.** Mechanical, and it would treat "ecosystem 7.0" as
  actionable. Rejected — see the framing.
- **Do nothing; it is one outside opinion.** Rejected: it is line-cited against the tree, it already
  drove closed items in the 2026-07-29 baseline table, and the SDK-defaults finding is real regardless
  of who noticed it.
- **Chase the score.** Explicitly rejected, and C-452 exists so the rejection is written down.

## Risks & open questions

- ⚠ **The biggest risk is winning the rubric and losing the product.** Every story here states which
  bucket it is in.
- ⚠ **C-444 changes SDK defaults, which is a breaking change for embedders** — and under the pre-1.0
  rule that is a MINOR. Defaulting to *safe* means existing embedders get behaviour they did not ask
  for; leaving it means the headline claim keeps a hole. Decide deliberately.
- **Open:** whether the review should be re-run after remediation, and by whom. A self-run re-review is
  worth much less than the original's two isolated contexts.
- **Open:** the review is a snapshot at 0.48.0 against Pi at v0.83.0. Both move.

## Acceptance / done

- Every finding in the review is in exactly one bucket, and the *defend* bucket has its reasoning
  written down where a reader will find it.
- An embedder cannot get auto-approval with no confinement and no resource ceiling by following the
  documented happy path.
- What a Windows deployment gets is stated, whether or not a backend is built.
- At least one head-to-head measurement exists, so the next comparison argues from numbers rather than
  from source reading.
