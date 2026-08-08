---
id: C-744
title: "A glossary the fleet's agents are held to"
pillar: "Core"
status: done
epic: delivery-is-verified
areas: [docs]
---

# A glossary the fleet's agents are held to

## Goal

The fleet's vocabulary is dense and precise — wave, park, claim, harvest, capture, fence, canonical
ref, member, lane, milestone, candidate, admitted operation — and agents demonstrably mangle it.
`AGENTS.md` keeps correcting the same confusions in prose: "a worker handoff is not Board
completion", "`already-built` matches a mention, not an implementation". Prose corrections do not
survive contact with a fresh agent; a checked glossary might.

## Acceptance

- [x] A glossary defines every term the board and fleet contracts depend on, in one place, with the
      distinction that makes each term non-obvious.
- [x] It is part of what an agent reads before acting, not a document it could skip.
- [x] A lint flags near-miss synonyms across stories — wave versus batch, park versus block, claim
      versus lock — because a story that renames a concept is how the vocabulary drifts.
- [x] The glossary changes in the same commit as a rename, so it cannot lag the code it describes.
- [x] **Failing-first**: `crates/flux-cli/tests/delivery_vocabulary.rs`. All three tests were run
      against the merge-base tree — `docs/glossary.md` absent, `AGENTS.md` and `docs/README.md`
      restored to their base state — and all three failed, on
      `read .../docs/glossary.md: No such file or directory` and on *"AGENTS.md is what every agent
      reads first; a glossary it does not name is one no agent reads"*. The backups were restored
      byte-identical and md5-verified.
      verify: `cargo test -p flux-cli --test delivery_vocabulary`

## Progress

- Done. `docs/glossary.md` defines 54 terms across the two systems, what is scheduled, what decides
  eligibility, what executes and what is produced. Every entry carries a `- Not:` line — the
  confusion it exists to prevent — and the lint refuses an entry that has none, because an entry
  without the distinction is a dictionary entry and the contracts already have those.
- Sourced from the contracts rather than invented: `AGENTS.md` in both repositories,
  `docs/board-and-fleet.md` and decisions 0003, 0010, 0013 (§4 defines milestone/lane/wave),
  0014, 0015, 0017 and 0021 in the roadmap repository, and the doc comments in
  `crates/flux-cli/src/board_fleet_cmd.rs`. Two terms were deliberately *not* defined: `seat`, which
  no contract uses (the word is `worker`), and `harvest`, which is recorded as having no verb rather
  than given an invented one.
- Required reading: `AGENTS.md` names it in the work contract, ahead of `git status`, and
  `docs/README.md` distinguishes it from `concepts.md` — product vocabulary there, delivery
  vocabulary here.
- It cannot lag: every entry anchors to a CLI operation checked against the binary's own schema, or
  to a token that must still appear in `board_fleet_cmd.rs`. Rename `fleet park` and the build fails
  until this file changes in the same commit.
- The synonym lint covers the three pairs the Acceptance names, and only the phrasings that can mean
  nothing else; the table at the end of `docs/glossary.md` is the list, and this note deliberately
  does not restate it, because the lint reads every story including this one. Matching is on whole
  words, so a longer word that merely ends in a banned one is not a finding.
- The bare words were measured before the table was written, and they are unusable as a lint:
  `batch` appears in 54 story files — the code itself calls the independent set a batch — `block*`
  in 136, `lock` in 69, and `tranche`, a genuinely retired word, in 10 historical documents nobody
  may edit. A lint people suppress is worse than none, so the narrowing is deliberate and its recall
  cost is real: a story calling a wave something loose in passing is not caught.
- Tests: `crates/flux-cli/tests/delivery_vocabulary.rs`.
