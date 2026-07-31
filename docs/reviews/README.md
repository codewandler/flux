# Reviews

Two kinds of document live here, and the directory a file sits in is its **state**, not its topic.

| Directory | Kind | What it is |
| --- | --- | --- |
| [`single/`](single/) | one review pass | One reviewer, one lens, one snapshot. Findings are numbered and self-contained. Written by the [`adversarial-review`](../../.agents/skills/adversarial-review/SKILL.md) skill or by a session retrospective. |
| [`aggregate/`](aggregate/) | a claim ledger | Normalizes the findings of *several* single reviews into one deduplicated, priority-ordered claim set, records where reviewers disagreed, and carries a validation status per claim. |
| [`archive/`](archive/) | handled single reviews | A pass that an aggregate ledger has normalized **and** whose residuals are filed as stories. Kept because the ledger cites it by `file:line` — archived means *settled*, never *deleted*. |

## Lifecycle

```
single/            →  aggregate/ normalizes it  →  residuals filed as stories  →  archive/
(status: open)                                                                   (status: handled)
```

A pass moves to `archive/` only when **both** halves are true: a ledger covers every one of its
numbered findings (the ledger's crosswalk table is the proof), and each surviving complaint has a
story or an explicit non-defect classification. Aggregating without filing is not "handled".

## Frontmatter

Every file carries a `triage:` block, so the state is readable without opening the directory listing.

A single review:

```yaml
triage:
  kind: single
  status: open | handled
  owner_stories: [C-186]        # stories tracking its findings
  aggregated_into: null         # or the ledger path, once one covers this pass
```

Once handled, it also records `triaged_on`, `normalized_claims` (the ledger IDs its findings became)
and `filed_as` (the epics and stories that own the residuals).

An aggregate ledger:

```yaml
triage:
  kind: aggregate
  date: 2026-08-01
  aggregates: docs/reviews/archive/
  filed_as: [C-345, C-352, …]
```

## Reading rules

- **A single review is evidence, not a verdict.** Passes examined different snapshots with different
  methods; agreement between two of them raises validation priority but does not make a claim true.
- **Don't re-triage an archived pass in isolation.** The ledger holds the cross-review disagreements
  and the anti-overclaim rules that no single document can see on its own.
- **A claim is not closed because code changed.** Closure needs the fixing commit *and* a regression
  test that fails when the fix is removed. The ledger's status vocabulary makes that distinction
  explicit — `historical-fixed` is a claim about evidence, not about a diff.
- **Line-number citations are load-bearing.** Archived files are cited by `file:line` from the ledger
  and from stories. Move or rewrite one and you break the audit trail; add new evidence in a new
  dated pass instead.

## Index

**Aggregate ledgers**

- [`2026-08-01-aggregate-complaint-triage.md`](aggregate/2026-08-01-aggregate-complaint-triage.md) —
  31 numbered findings from five passes normalized into 25 claims, validated against the tree on
  2026-08-01, residuals filed as C-345, C-352, C-358, C-363, C-369, C-375 and C-382.

**Open single reviews**

- [`2026-07-29-security-posture-desk-review.md`](single/2026-07-29-security-posture-desk-review.md) —
  external, security and production readiness. The baseline every later security pass diffs against.
- [`2026-07-29-envelope-integrity.md`](single/2026-07-29-envelope-integrity.md) — is there a path to
  effect that skips the envelope?
- [`2026-07-30-security-assurance-closure.md`](single/2026-07-30-security-assurance-closure.md) —
  the 2026-07-29 baseline re-verified against the shipped tree.

Tracked by C-186 (in progress) and C-267.

**Archived** — five passes, all normalized into the 2026-08-01 ledger: three independent adversarial
security reviews of commit `cb3bb057` (2026-07-30) and two harness/tooling friction retrospectives
(2026-07-31).
