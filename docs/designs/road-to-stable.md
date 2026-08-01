# Design: The road to stable — what must be true before flux is measured rather than built

**Status:** proposed · **Pillar:** Core · **Stories:** see [§ Stories](#stories)

## Why

flux has 663 done stories and ~110 open. The open set is not a queue of equal work: **roughly 16
stories block a credible claim that flux is stable, and the other ~94 are capability that stability
does not depend on.** This epic names those 16 so the distinction stops being re-derived.

The organizing idea is a change in *mode*. Today flux is built: stories come from a roadmap, get
implemented, get reviewed. The intended next mode is **measured**: harness runs (`flux-bench`) drive
improvement, and regressions are read off benchmarks rather than found by reading code. That switch
is only safe once two things hold — the runtime does the right thing, **and** the API you benchmark
against stops moving underneath the measurements.

### The evidence that this is the right moment to ask

Three findings from the 2026-07-31 backlog analysis, all measured rather than felt:

**The backlog has flipped from planning-driven to discovery-driven.** Stories whose origin is a
review or an implementor report, banded by ID:

| Band | Review-origin | Share |
|---|---|---|
| C-1…200 | ~1/192 | ~1% |
| C-201…300 | 22/99 | 22% |
| **C-301…340** | **19/35** | **54%** |

**Zero of the 20 newest stories are new capability.** All twenty trace to a review, an implementor
report, a corpus, or a measured failure. That is what a system looks like when it stops being built
and starts being interrogated — a healthy sign, but it means discovery is still expanding *within*
already-identified classes. C-340's own note: *"C-301 was the tip: durations were one construct, this
is six more."*

**The defect rate is tractable and clustered.** 17 of 85 non-epic open stories (20%) describe a defect
a user could hit today, and they group into three places: the webhook/connector delivery surface, the
Flux-Lang grammar and its editor mirrors, and the redaction path. Two of the redaction stories fail
*open*.

### The definition already exists in this repo

[C-255](../stories/C-255-adversarial-review-remediation-epic.md) has one open acceptance bullet, and it is
the honest definition of stable:

> Three fresh independent reviews against the resulting exact working tree find no reproducible
> High-severity containment defect.

It is not ticked. C-255's own progress log records that its first closure pass found **twelve**
reproducible High/Medium defects *after* every child story was marked done. That is the bar, and it
has already refused to be cleared once.

## Approach

Close the three defect clusters, then settle the published surface. In that order, because the second
is a scheduling decision that only makes sense once the first is not moving.

### 1 · The clusters (defects a user can hit)

- **Redaction — and this is the sharpest thing in the tree.** [C-339](../stories/C-339-redaction-falls-back-to-the-unredacted-value.md)
  fails **open**: when text-level redaction corrupts the JSON badly enough that it stops parsing,
  `unwrap_or(canonical)` hands back the **unredacted** value. Silent, on a published SDK path.
  [C-338](../stories/C-338-four-copies-of-the-total-walk.md) removes the duplication that let the
  class recur four times.
- **Grammar + editor mirrors.** [C-340](../stories/C-340-grammar-cannot-parse-half-the-canonical-corpus.md)
  is priority 1: the shipped grammar produces 166 `ERROR` nodes across 7 of 15 canonical examples, so
  flux's own documented language is unhighlightable in the editors it ships grammars for.
  [C-336](../stories/C-336-named-argument-values-highlight-as-punctuation.md) is the same surface one
  layer in.
- **Webhook / connector delivery — the largest unaddressed cluster, and the only one on an
  internet-facing port.** No signature verification ([C-291](../stories/C-291-webhook-verify-raw-body.md),
  [C-292](../stories/C-292-webhook-signature-schemes.md)), no envelope and a symbol-shadowing payload
  bind ([C-295](../stories/C-295-delivery-envelope-verified-flag.md)), and an unpoliced second request path
  ([D-217](../stories/D-217-channel-reply-through-the-executor.md)). **None of the four is prioritized** —
  that is itself the finding.
- **Unattended runs.** [C-227](../stories/C-227-no-automatic-resume-on-transport-class-provider-failure.md) /
  [C-228](../stories/C-228-gemini-3x-over-openrouter-drops-the-stream-mid-exploration.md): a dropped provider stream ends a whole run.
  Until this closes, "unattended" is not a supported mode — and harness runs are unattended by
  definition, which makes this a *precondition* for the benchmark-driven mode rather than a nice-to-have.

### 2 · Test integrity (why a green benchmark would mean something)

A benchmark-driven mode is only as trustworthy as the guards under it. Two general mechanisms already
landed — [C-328](../stories/C-328-pin-census-wiring-declares-its-test.md)'s pin census for unobserved
wiring, and [C-334](../stories/C-334-tree-sitter-corpus-check.md)'s corpus check for the editor
mirrors. What remains is the tail: [C-314](../stories/C-314-limits-wirings-nothing-observes.md)
(two items still open after C-328 closed its first), [C-313](../stories/C-313-url-encoder-consolidation-and-key-pinning.md),
and [C-332](../stories/C-332-home-reading-tests-need-an-injection-seam.md) — 53 tests whose verdict
depends on the developer's `$HOME`.

### 3 · The published surface (the real blocker)

The **architecture** is settled: [C-337](../stories/C-337-architectural-simplification-epic.md) says
explicitly *"preserve, do not redesign."* The **API** is not. C-337 records, in writing, a scheduled
breaking window for `AgentSpec`, compatibility doors slated for deletion in "one planned minor
release", 37 crates with no current ownership audit, and eight files between 3,014 and 9,789 lines —
**and it carries zero implementation stories.**

At least six open stories would each move a published contract: C-337 (`AgentSpec`), A-103
(`ToolSpec` gains `Compensation`), A-104 (`EventKind` is a closed set — a new variant is a durable
schema bump), C-338 (forces a `pub` item on the published `codewandler-flux-secret` or a new
dependency edge), C-244 (A2A `Task.artifacts`), C-295 (`Event` shape).

**This is why "stable" cannot be declared on bug count alone.** Benchmarking against an API with a
deliberate break still queued makes regressions indistinguishable from intended churn.

## Alternatives considered

- **Declare stable on the defect count alone.** Rejected: it is the weaker half. A benchmark suite
  built against a surface that then takes its planned breaking window produces a step change in the
  numbers that nobody can attribute.
- **Take the breaking window first, then fix defects.** Rejected: C-337 has no implementation stories
  yet, and its own acceptance says the `AgentSpec` migration needs a design before a window is
  scheduled. Sequencing the break before the cluster work would mean breaking the API and *then*
  discovering more defects underneath it.
- **Freeze the API now without C-337's cleanup.** Rejected: that locks in the compatibility doors and
  the positional assembly paths C-337 exists to remove, permanently.
- **Treat C-255's closure bullet as satisfied by the work already done.** Rejected on this repo's own
  evidence: the first closure pass found twelve reproducible defects *after* every child was done.
  The bullet asks for fresh independent reviews, and that is the point of it.

## Risks & open questions

- **The cascade may not be finished.** C-315 → C-323 → C-338/C-339 and C-301 → C-334 → C-340 each
  found *more* than their parent. The redaction and grammar clusters are both mid-cascade; closing
  the listed stories may surface successors. That is the argument for C-255's "three fresh reviews"
  bullet rather than a checklist.
- **The webhook cluster is unprioritized and unstarted**, and it is the only cluster on an
  internet-facing port. Deciding its priority is the first action this epic should force.
- **C-337 is undecomposed.** Until it has stories, its breaking window cannot be scheduled, and this
  epic cannot close.
- **`areas:` under-reports the defect map** — only 62 of 110 open stories carry the field, and 7 of
  the 20 correctness stories have none (the whole webhook cluster among them). Any future analysis
  keyed on `areas:` will understate exactly the cluster that matters most.
- Open question: does "stable" here mean **1.0**, or a pre-1.0 marker? flux uses the minor position
  as the breaking signal pre-1.0; if stable means 1.0, the API-surface work is not optional and the
  `AgentSpec` window must land first.

## Acceptance / done

This epic is done when all of the following hold — each is checkable, not a judgement:

- [ ] No open correctness story **fails open**. (C-339; C-323 ✅ closed 2026-07-31.)
- [ ] No open priority-1 or priority-2 correctness story. (C-340.)
- [ ] The editor mirrors are guarded and the guard is not vacuous. (C-334 ✅; C-340, C-336.)
- [ ] Every channel delivery is authenticated and distinguishable from an unverified one.
      (C-291, C-292, C-295, D-217 — and all four are prioritized rather than sitting in `backlog`.)
- [ ] An unattended run survives a provider transport failure. (C-227, C-228, epic C-229.)
- [ ] No wiring line without an observing test. (C-328 ✅ shipped the census; C-313, C-314, C-332.)
- [ ] The outstanding advisory is cleared. (C-205.)
- [ ] Vendor-host reach is **disclosed** at approval when flux is not the one dialing, and bounded by
      the manifest's own allowlist. (C-311 ✅ shipped.) ⚠ Deliberately *not* "egress holds": the
      declaration is re-verified against the allowlist and cannot be shed at refresh, but a
      deployment that declares one host and dials another is outside what any check on flux's side
      can catch. Restoring an enforced bound needs flux to see the vendor URL, which the credential
      boundary exists to prevent.
- [ ] **C-337 is decomposed into stories and its breaking window is scheduled.** Until this, "stable"
      can mean "the runtime does the right thing" but not "the API you build against will not change."
- [ ] **C-255's final bullet is ticked**: three fresh independent reviews against the exact resulting
      working tree find no reproducible High-severity containment defect.

## Stories

⚠ **`epic:` is single-valued, so this epic does not own all of its blockers.** Six of the fifteen
already belong to an epic that describes them better than "stability" does, and they were left there
rather than re-homed — an epic tag records *where work is organised*, not *what it blocks*. They are
listed below as **cross-referenced**, and this epic's acceptance depends on them exactly as much as
on the ones it owns:

| Story | Its epic |
|---|---|
| C-291, C-292, C-295 | `verified-webhook-channel` |
| D-217 | `connector-channels` |
| C-227, C-228 | `unattended-run-integrity` |
| C-311 | `connector-platform` |
| C-205 | `security-assurance` |

Everything else below carries `epic: road-to-stable`. Listed by the order this epic argues for, not
by ID.

**Fails open — do first**

| Story | Title |
|---|---|
| C-339 | When redacted text stops parsing, `redact_and_hash_request` returns the *unredacted* value |
| C-338 | Four copies of the same total-walk redaction logic, which is how the node-kind hole recurred |

**Grammar and editor mirrors**

| Story | Title |
|---|---|
| C-340 | The tree-sitter grammar cannot parse 7 of 15 canonical examples — 166 ERROR nodes |
| C-336 | A named-argument value highlights as punctuation, and swallows the comma after it |

**Webhook / connector delivery — unprioritized, internet-facing**

| Story | Title |
|---|---|
| C-291 | `channel webhook` — capture the raw body and verify a declared signature before parsing |
| C-292 | Webhook signature schemes — one parameterized HMAC, constant-time, replay-bounded |
| C-295 | The delivery envelope — an Event carries no id, no source and no `verified` flag |
| D-217 | A channel can call an operation — `Deliverer::call_operation` through the full safety envelope |

**Unattended runs — a precondition for harness-driven work**

| Story | Title |
|---|---|
| C-227 | A dropped provider stream ends the whole turn — no automatic resume for a transport-class failure |
| C-228 | Gemini 3.x over OpenRouter drops the stream mid-exploration, reproducibly |

**Test integrity — what makes a green benchmark mean something**

| Story | Title |
|---|---|
| C-332 | 53 of 73 `HOME`-reading tests have no injection seam and no story |
| C-314 | Two `[limits]` wirings nothing observes, and an occupancy test that guards less than its prose |
| C-313 | The fifth encoder copy, and the query key nothing pins |

**Envelope and hygiene**

| Story | Title |
|---|---|
| C-311 | Vendor-host disclosure at approval — show what an op reaches when flux is not the one dialing |
| C-205 | Bump lru to >= 0.16.3 and drop the RUSTSEC-2026-0002 advisory ignore |

**Already closed, listed because the acceptance references them**

| Story | Title |
|---|---|
| C-323 ✅ | `redact_json` skips `Value::Number`, and an all-digit credential has no recourse but registration |
| C-328 ✅ | A wiring line declares the test that observes it — the pin census |
| C-334 ✅ | Nothing verifies that the pinned tree-sitter rev parses canonical Flux |

**Referenced epics, not members** (an epic is not nested inside another)

- [C-255](../stories/C-255-adversarial-review-remediation-epic.md) — adversarial review remediation; its
  final bullet *is* this epic's last acceptance item.
- [C-337](../stories/C-337-architectural-simplification-epic.md) — architectural simplification;
  undecomposed, and the reason the published surface is still moving.
- [C-229](../stories/C-229-unattended-run-integrity-epic.md) — unattended run integrity; parent of
  C-227/C-228.
