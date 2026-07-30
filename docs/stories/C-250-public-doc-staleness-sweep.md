---
id: C-250
title: "Public docs describe a product that has moved — sweep for staleness and keep the enumerations honest"
pillar: Core
status: ready
priority: 9
areas: [website]
note: "the published board pages listed seven generated ops when the code shipped nine, within hours of the change — closed enumerations in public docs rot silently because nothing tests them"
---

# Public docs describe a product that has moved — sweep for staleness and keep the enumerations honest

## Goal
Find and fix places where flux's **public** documentation no longer describes the shipped product,
using two angles: docs whose subject code has moved since the doc was last touched, and anything
user-visible added since the last feature release with no public doc at all.

The motivating finding, and the reason this is a recurring hazard rather than a one-off: after C-236
added `board.query` and `board.comments`, the published pages still enumerated a **closed list of
seven** generated board ops when the code generated **nine** — and the two missing ones were exactly
the machine-readable pair a Program consumes as data. A reader following the docs had no way to learn
the capability existed. Nothing failed, because **no test pins those enumerations**; only
`website/docs/language/ops.md` is checked against the live registry, and generated board ops do not
appear in it at all.

## Acceptance
- [x] `website/docs/agent/fleet.md` and `website/docs/agent/datasources.md` enumerate all nine
      generated board ops, and document `query`'s typed rows, the `depends_on` filter and the
      `comments` read-back — with examples verified against the shapes the runtime actually accepts
      (filters nest under `filters`; an `each` source must be a bound value, not a call).
- [x] `website/docs/language/ops.md` states the new selection-op string rule (`regex_extract` single
      match, `first`, `last`, `coalesce` hand back the bare string) **and** its boundary — non-strings
      still come back as JSON, and `split`/`keys`/`all: true` are unaffected.
- [x] `website/docs/language/ops.md` carries a prominent caution for the `git_revert` → `git_reset`
      rename, naming the hazard that the old call *still looks valid* while doing something different.
- [x] The `board.query` row enumeration is complete — it listed eight fields while `item_row` emits
      nine, omitting `attempts`, next to a sentence asserting "every row carries every field".
- [ ] Sweep the **remaining** public surface not yet covered: `README.md`, `docs/usage.md`, and the
      rest of `website/docs/**` beyond the board/ops pages.
- [ ] Decide whether the closed enumerations that caused this are worth pinning. A doc listing "the
      N generated board ops" cannot be checked by `website_contract` today because generated
      datasource ops never enter the builtin catalog it walks. Either pin them or state why prose that
      cannot be tested is acceptable there — the point is that the choice becomes deliberate.
- [x] Standard gate green (the two suites that pin `ops.md` against the registry:
      `flux-cli --test website_contract` 18/18, `flux-tools --test toolspec_invariants` 5/5, plus
      `flux-lang --test website_in_sync` 3/3).

## Progress
- 2026-07-30 — **first pass merged.** Left `ready` rather than `done`: the four ticked items shipped,
  the remaining public surface (`README.md`, `docs/usage.md`, the rest of `website/docs/**`) and the
  decision about pinning enumerations are still open.
  Verified at integration: `flux-cli --test website_contract` 18/18,
  `codewandler-flux-tools --test toolspec_invariants` 5/5,
  `codewandler-flux-lang --test website_in_sync` 3/3. A full workspace gate was **not** re-run for this
  merge and did not need to be — the diff is markdown under `website/docs/` only, so build, clippy, fmt
  and codegate results carry over unchanged from C-230's green gate on identical code; what a docs
  change can break is the suites that read those files at runtime, and those are the three above.
  No `WHATS-NEW.md` entry: the capabilities themselves were already announced there under C-236 and
  C-238, and this is the reference documentation catching up, not a product change.
- 2026-07-30 — first pass landed on `docs/staleness-sweep-0.36` (three commits). The sweep agent was
  stopped part-way; its work was preserved and completed by the coordinator, which is how the
  `attempts` omission was caught — **the sweep's own output had the same defect class it was opened to
  fix**, an enumeration the code had outgrown. Worth remembering as a review rule: a doc fix that
  lists fields or ops needs the same check against the code that the original doc failed.
- 2026-07-30 — four claims verified mechanically against the code rather than by eye:
  `DependencyMatch::ALL` is `[Satisfied, Unsatisfied]`; exactly nine board ops are generated; `filters`
  nests under `additionalProperties: false`; and `item_row` emits every field, so unset optionals
  serialise as `null`.

## Notes
- **Why this recurs:** the public site has real contract tests, but they cover only the *builtin* op
  catalog and the generated prelude/node-kind blocks. Hand-written prose that happens to enumerate
  something — ops, fields, states — is unguarded, and enumerations are exactly what goes stale when a
  capability is added. That asymmetry is the durable finding, not any individual wrong list.
- `WHATS-NEW.md` and `website/docs/whats-new.md` are out of scope here: the mirror is generated and
  pinned by `website_customer_changelog_is_in_sync`, and the customer entries for this cycle
  (including the `git_reset` rename under "Action needed") are already written.
- Voice rules for anything public are codified in the HTML comment at the top of `WHATS-NEW.md`:
  plain language, feature-first, no story IDs, no crate names.
