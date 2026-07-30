---
id: L-92
title: Named one-shot flow entrypoints for flux run
pillar: Language
status: done
epic: zendesk-automation
design: docs/designs/zendesk-automation.md
areas: [flux-cli, flux-lang]
note: "`flux run module.flux --entry name --inputs/--arg` reuses the direct-flow engine; no-entry app behavior is unchanged"
---

# Named one-shot flow entrypoints for `flux run`

## Goal

Let one Flux-Lang module expose several scriptable entrypoints without turning the file into an
interactive app or splitting it into one file per action.

## Acceptance

- [x] Failing-first CLI tests prove `--entry` selects exactly one top-level flow from a multi-flow
      module and `--inputs` / repeatable `--arg` follow the existing strict flow-input contract.
- [x] The selected flow executes through the direct authored-flow engine and full safety envelope,
      including module-local composite ops, then exits.
- [x] Unknown entries/inputs, missing inputs, journeys, prompts, trailing words, and stream-JSON
      combinations fail before session/provider construction.
- [x] Omitting `--entry` preserves the existing `.flux` app/program path byte-for-byte in behavior.

## Progress

- 2026-07-30 — story filed; implementation starts with CLI parse and selection tests.
- 2026-07-30 — shipped `--entry`, `--inputs`, and repeatable `--arg`; named selection and positional
  validation happen before provider/session construction and reuse the direct authored-flow engine.
  Focused CLI tests and the full root build pass.
