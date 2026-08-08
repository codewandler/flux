---
id: C-665
title: "The board records what a scout saw, and what it still owes"
pillar: "Core"
status: backlog
priority: 11
epic: scouting-makes-the-backlog-schedulable
areas: [flux-cli]
note: "1182 stories, 471 declare areas; the other 711 are held back as an undeclared write set and no verb can fix that after creation"
---

# The board records what a scout saw, and what it still owes

## Goal

Give the board somewhere to put what a scout learns, and a way to say what it has not learned yet.

Today an item's write set can be declared exactly once. `flux board create` accepts `--area`,
`--epic`, `--design`, `--priority` and `--note`; `flux board update` accepts only `--title`,
`--priority` and `--note`. **So `areas` can be set at creation and never changed.** The same is true
of `depends_on`, which has no flag on either verb.

That is why the number is what it is: of 1182 stories, **471 declare `areas` and 711 do not**.
`wave_conflict` reads an empty `areas` as `"undeclared write set"` and holds the item out of every
independent batch, so those 711 are unschedulable at width — not because they collide with anything,
but because nobody said what they touch, and nothing can say it for them now.

This story adds the annotation surface and the debt counter. No agent, no model calls: it is the
substrate the scout writes through, and it is useful on its own, because it turns "711 undeclared"
from a fact nobody can see into a number that moves.

## Acceptance

- [ ] `flux board update` can set `areas` and `depends_on` on an existing item, validated the way
      `board check` already validates them, so a wrong area is a refusal rather than a bad batch.
- [ ] An item carries a `scout` stamp recording the commit scanned, the board revision, the agent,
      and a digest of the story body.
- [ ] A stamp is **stale** when the body digest no longer matches, or when a declared area names a
      path the repository no longer has. Staleness is derived, never stored, so it cannot drift.
- [ ] `flux board next` reports the scouting debt — unscouted plus stale — and each held-back item
      names which of the two it is, alongside the collision reasons `held_back` already gives.
- [ ] `flux fleet status` carries the same count, so the debt is visible without a board query.
- [ ] The annotation surface is a **native operation**, so an authored loop can call it. Setting it
      is a mutation in `board schema` and honours `--dry-run` and `--if-revision`.
- [ ] Failing first, a test proves an item whose body changed after its stamp is reported stale while
      remaining exactly as it was on the board — the derivation must not mutate what it measures.

## Notes

- **Frontmatter, not a sidecar.** `areas` and `depends_on` must stay in frontmatter because
  `select_independent_batch` already reads them there. A second home means a second copy of the
  collision logic, and the two will disagree.
- Depends on nothing, but everything else in this epic depends on it.
- `crates/flux-cli/src/board_fleet_cmd.rs` holds the `Story` struct, the frontmatter parsing, the
  batch selector and `board next` — all of it in one 17k-line file. See `C-657`: until that splits,
  this story serialises against every other story that touches a board or fleet verb.
