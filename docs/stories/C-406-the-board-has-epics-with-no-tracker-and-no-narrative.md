---
id: C-406
title: "Five epic slugs carry open stories with no tracker story, and twenty-two have no roadmap narrative"
pillar: Core
status: ready
priority: 14
epic: road-to-stable
areas: [docs]
note: "found by a mechanical audit of all 862 stories on 2026-08-01. The board is internally consistent — no malformed frontmatter, no duplicate ids — but the epic layer has drifted: `network-primitives` has 5 open stories and no tracker at all, and ~50 open stories sit under epics the roadmap never describes"
---

# The epic layer has drifted from the story layer

## Goal

Make the epic layer answer the question it exists to answer — *what is this initiative and why* —
for every epic that currently has open work.

An audit of all 862 stories on 2026-08-01 found the story layer healthy: no malformed frontmatter,
no duplicate IDs, every `ready` story carries a priority. The **epic** layer is where the drift is.

## What the audit found

**1. ⚠ Five epic slugs have open stories but no epic tracker story at all.** Nothing states what the
initiative is, so the only way to learn it is to read its members and infer:

| epic slug | open stories |
|---|---|
| `network-primitives` | 5 |
| `verified-webhook-channel` | 4 |
| `connector-platform` | 1 |
| `connector-backed-storage-facade` | 1 |
| `flux-planner-ship` | 1 |

`connector-platform` is the sharp one despite its count: it carried C-311, C-312, C-403 and C-404 —
the whole credential-boundary arc — which all closed on 2026-08-01. An epic that just absorbed four
safety-envelope stories and has no tracker is the case where the missing narrative costs most.

**2. Twenty-two epics have no narrative in `docs/roadmap.md`**, covering roughly 50 open stories. The
largest: `agent-change-recovery-and-provenance` (9), `egress-pinning-and-confinement-residuals` (7),
`harness-route-integrity` (7), `release-trust-residuals` (6), `structural-gate-blind-spots` (6),
`serving-surface-and-turn-outcome-residuals` (6), `connector-channels` (6).

⚠ **Judgement required, not a mass edit.** Several of those are *review-remediation buckets* — a
grouping for findings from one adversarial pass — and a roadmap narrative for a bucket may be the
wrong artifact. The decision this story wants is: which of the 22 are genuine initiatives that owe a
narrative, and which are buckets that should say so. Do not write 22 narratives.

**3. A dangling reference.** `docs/stories/C-363-*.md` cites `C-330 widens it`; no C-330 exists.
Either it was never filed or the ID is wrong. (Checked: `C-1…200` in C-342 is a range, not a
reference — not a defect.)

**4. 185 non-`ready` stories still carry a `priority` field**, which the schema says to omit outside
`ready`. Inert today, because the board is generated from `status` — but a story reopened from `done`
to `ready` silently inherits a stale rank.

**5. Priority is not a total order among `ready` stories.** Nine values are shared by two or more
stories; priority 10 is held by five (C-219, C-251, C-399, C-404, L-103) and priority 9 by four.
`/track:next` picks the top `ready` story by priority, so within a collision the choice is arbitrary
— the ordering does not actually order.

## Acceptance

- [ ] The five epics without a tracker either get one, or their stories are re-pointed at an epic
      that exists. `connector-platform` first — it has the most closed history to summarize.
- [ ] Each of the 22 narrative-less epics is **classified**: initiative (owes a roadmap narrative) or
      remediation bucket (says so in its tracker, and the roadmap does not pretend otherwise). Record
      the classification, so the next audit does not re-derive it.
- [ ] C-363's `C-330` reference is resolved — the story filed, or the reference corrected.
- [ ] Decide whether stale `priority` on non-`ready` stories is worth a sweep. A 185-file mechanical
      edit has a real cost in history noise; "leave it, the board ignores it" is an acceptable
      outcome **if written down** so the next audit does not re-flag it.
- [ ] Priority collisions among `ready` are either broken (made a total order) or the schema is
      amended to say priority is a *band* rather than a rank. Do not leave the doc claiming a rank
      that the data does not have.
- [ ] The audit is repeatable — ship the check, or record the queries, so this is a standing property
      rather than a one-off snapshot.

## Notes

- ⚠ A methodology note for whoever re-runs this: an early version of the audit used `^field:\s*(.*)$`
  to read frontmatter. `\s` matches newlines, so an **empty** field (`priority:` with no value)
  captured the *following* line and produced 319 phantom findings. Use `[ \t]*`. The numbers above
  are from the corrected pass.
- The board generator is `python3 …/track/0.5.0/scripts/gen_board.py docs` and is deterministic; it
  was current at the time of the audit, so none of the above is board staleness.

## Progress

- Filed 2026-08-01 from a mechanical audit of all 862 stories, run during a curation pass.
