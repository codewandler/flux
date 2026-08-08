---
id: C-741
title: "A story declares its kind and is validated as that kind"
pillar: "Core"
status: done
epic: delivery-is-verified
areas: [flux-cli]
---

# A story declares its kind and is validated as that kind

## Goal

Every story is validated as though it were a feature. A spike's output is a decision, an enabler's is
capability, a bug's contract is current-versus-expected behaviour — judging any of them against
"behaviour implemented with a failing-first test" is a schema lie, and it pushes authors toward
writing criteria they do not mean.

## Acceptance

- [x] A story declares `kind: feature | enabler | spike | bug`, defaulting to `feature` so existing
      stories are unaffected.
- [x] Validation follows the kind. A spike is not required to name a failing-first test; a bug states
      current and expected behaviour; an enabler names the capability it unlocks.
- [x] The kind is visible to the driver, so dispatch and review apply the rules that fit the work.
- [x] Regression test: a spike with no failing-first test passes `check`, and a feature without one
      does not.

## Progress

- `Story.kind` is always resolved, never absent, so every reader is handed a kind instead of having
  to know the default. It rides the existing serialization: `board items|get|next|query` and
  `fleet inspect story`. The reviewer's packet carries `contract.kind`, read from the reviewed
  commit like every other field in it, and `reviewer_instructions` says what each kind means.
- Enforcement is on the `-> ready` edge, shared with C-740's marker refusal. `board check` holds a
  story to its kind's contract only when the story *declares* a kind: the 1,260 stories written
  before the field default to `feature` for every other purpose but are not failed retroactively
  against a rule they were never written to. Verified: `board check` over the real board reports
  exactly what it reported before this change — 1,260 stories, one pre-existing `C-320` warning.
- The phrasing sets in `kind_contract` are a floor on what a contract *states*, not a judgement of
  how well it states it. `--override-reason` records the cases they misjudge.
- Measured impact of the feature clause: 95 of 221 `backlog` stories do not currently name a
  failing-first test, so promoting one now requires stating it or recording an override.
