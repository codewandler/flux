---
name: adversarial-review
description: >-
  Run an adversarial, evidence-anchored review of flux — security posture, production readiness, or a
  single subsystem — and land it as a dated, verifiable artifact under reviews/. Use this when asked
  to review/audit/red-team flux or one of its crates, to assess production or security readiness, to
  challenge the safety envelope, to verify an external review against the tree, to re-run a prior
  review against a newer version, or to answer "is flux actually safe to run unattended?". Not for
  ordinary diff review — use /code-review for that.
---

# Adversarial review of flux

## What this is for

Ordinary review asks *"is this change correct?"*. This asks a harder question:

> **Where does flux's actual behaviour fall short of what flux claims about itself?**

flux makes an unusually strong claim — *the LLM is not the runtime; every operation traverses
authorization → approval → guarded IO; there are no bypass paths*. A claim that strong is the review
target. The job is to find the gap between the claim and the tree, state it with evidence, and rank
it honestly.

**The default posture is disbelief.** README, AGENTS.md, design docs and doc-comments are *claims to
be tested*, never evidence. Only code, tests, CI config and release workflows are evidence. When a
doc and the tree disagree, **the tree wins and the disagreement is itself a finding**.

## The prime directive: evidence or it doesn't exist

Every finding carries a `path:line` you actually opened. No exceptions, no "appears to", no
"likely". If you could not reach the evidence, the finding is **not a finding** — it is an *open
question*, and it goes in a separate section under that name.

This is the single rule that separates a useful review from LLM-flavoured security theatre. A
plausible-sounding unverified claim is worse than silence: it burns maintainer time and teaches the
reader to distrust the whole document.

**Corollary — grade against invariants, not vibes.** Before filing anything, ask what invariant it
actually breaks. "No global request-body limit" is real. "Uses `unwrap()` in a test" is not. Raw
adversarial passes systematically over-severitize; correct for it *before* writing, not after.

## Scope: pick a lens, say so

Full-repo reviews at every lens at once produce mush. Pick, and declare the pick in the frontmatter:

| Lens | Asks |
| --- | --- |
| `security-and-production-readiness` | Can this be trusted with a real repo, unattended? *(the default)* |
| `envelope-integrity` | Is there a path to effect that skips authorization → approval → guarded IO? |
| `supply-chain` | Can an attacker influence what a user actually executes? |
| `subsystem` | One crate, exhaustively — name it |

## Method

### 1. Establish ground truth first

Before reading a line of prose, pin what you are reviewing. Version (`Cargo.toml`), latest tag
(`git tag --sort=-v:refname | head -1`), crate count, workspace shape. A review that does not say
what it reviewed cannot be re-run or diffed later.

### 2. Walk the claim surface, then attack it

The claims live in README.md, AGENTS.md, SECURITY.md, docs/architecture.md, docs/vision.md. Extract
them as *testable propositions* ("policy evaluation is default-deny", "approved batches are
rechecked at dispatch"). Then go find where each is enforced — and where it isn't.

### 3. Hunt in the places that have historically hidden things

These are flux's load-bearing seams. Each has produced a real finding before:

- **Defaults vs. capability.** flux implements strong protections and then ships them *opt-in*. The
  gap between "supported" and "on" is the most productive seam in the repo. Check
  `crates/flux-system/src/sandbox.rs` — mode default, network default, degrade-vs-fail behaviour,
  which platforms have a backend at all.
- **The envelope's edges.** Not the happy path — the edges. Direct router mounts that skip a
  `serve_on` guard. Sub-agents, plugins and hooks inheriting authority. Anything that constructs a
  guarded-IO handle without going through the policy layer.
- **Classification trust.** The policy engine is only as good as each operation's self-declared
  read-only/effectful flag, risk level, permission subjects and authority requirements. A
  misclassified tool silently skips the approval it deserved. In a fast-growing registry this is a
  standing trust assumption, not a solved problem.
- **Normalization.** Path canonicalization before matching, deny-first ordering, DNS-pinning after
  resolution, redirect revalidation per hop. These are where "we check it" quietly becomes "we check
  something adjacent to it".
- **Server surface.** `crates/flux-server/` — auth mode vs. bind address, body limits, request
  timeouts, rate limiting, what `--yes` composes with.
- **CI and release.** `.github/workflows/` — grep for what *isn't* there: advisory scanning
  (`cargo-audit`/OSV), `cargo-deny`, SAST, fuzzing, Miri, provenance attestation. Check whether
  third-party actions are pinned to SHAs or to movable tags. Check whether release binaries are
  signed or merely checksummed.

Absence findings are as legitimate as presence findings — and in a young repo, usually more useful.
`grep` returning nothing **is** evidence, provided you say what you grepped for.

### 4. Rate on axes, not as one number

A single score hides the actual story. The useful signal in the 2026-07-29 baseline was that
*architecture rated 8 while assurance rated 5* — the finding is the spread, not either number.
Use these axes, and keep them stable across reviews so scores are comparable over time:

`security_architecture` · `secure_defaults` · `implementation_quality` · `security_assurance` ·
`release_supply_chain` · `product_maturity` · `community_bus_factor` · `production_readiness`

### 5. Separate what a code change can fix

Bus factor, adoption and external-audit history are real risk and belong in the review — but they are
context, not defects. Say which findings are actionable and which are structural, so the maintainer
does not read an unfixable line as a to-do.

## Deliverable

One file: `reviews/YYYY-MM-DD-<slug>.md`. Never overwrite a prior review — reviews are a time series,
and their value is the diff between them.

Required frontmatter (mirror `reviews/2026-07-29-security-posture-desk-review.md`):

```yaml
---
title: <what was reviewed, at what lens>
date: YYYY-MM-DD
kind: external-review | internal-review | subsystem-review
lens: <one of the lenses above>
method: <what you actually did — and did NOT do: no fuzzing, no exploitation, no runtime testing>
reviewer: <external | internal | agent>
subject:
  repo: codewandler/flux
  version_in_tree: <Cargo.toml version>
  published_release_at_review: <latest tag>
  workspace_crates: <n>
overall_rating: n/10
verdict: <one line a reader can quote>
ratings: { <the eight axes> }
verification:
  status: <verified against tree at <version> on <date> | unverified>
  outcome: <what the verification resolved to>
  material_errors: <none | list>
top_findings: [ <3-6 one-liners, most severe first> ]
---
```

Body: **Verdict** → **Ratings table** → **Strengths** (real ones, stated as specifically as the
criticisms) → **Findings**, severest first, each with `path:line` → **Open questions** (the things you
could not verify — never silently dropped) → **Deployment recommendation**, if the lens warrants it.

## Verifying someone else's review

When the input is an external review rather than your own pass, the work is *verification*, and the
rules change:

1. **Never edit their text.** Their words stay byte-identical. You append.
2. Append a `# Verification against the tree` section. Every load-bearing claim becomes a row:
   claim | ✅ confirmed / ❌ refuted / ⚠️ stale | the `path:line` that settles it.
3. **Refuting a claim is the highest-value output of the pass.** A confirmed review is a to-do list;
   a refuted claim prevents wasted work. Look for them deliberately — do not confirm by default
   because the reviewer sounded confident.
4. Distinguish *wrong* from *stale*. A version-drift observation written the day a release was cut is
   a timestamp artifact, not an error. Mark it `⚠️ stale-by-design` and say why.
5. Close with **What this changes** — which findings are cheap assurance wins, which are product
   decisions needing a design trail, which are context.

## Boundaries

- **This is a desk review.** Read the source; do not fuzz, exploit, attack live infrastructure, or
  weaken a guard "to see if it's reachable". State that limitation in `method:` every time.
- **Review, don't repair.** A review that quietly fixes things cannot be audited. Findings that
  deserve work become stories via `/track:story` — that is the handoff, and it is the user's call.
- **Never commit.** Per `AGENTS.md` and the user's standing rule, commits happen only on explicit
  instruction.
- **No consumer internals.** Reviews are repo artifacts — say "the downstream consumer", never name
  a specific customer or internal system.

## Baseline

`reviews/2026-07-29-security-posture-desk-review.md` — external, `security-and-production-readiness`,
flux `0.33.1`, `6/10`, *"Promising security-engineered beta — not yet a trusted security boundary."*
Fully verified against the tree; no material errors. Diff any later review against it and lead with
what moved.
