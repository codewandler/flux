# flux — backlog & status board

The single screen for **"what to work on next and where we are."** One file per story lives in this
directory (`<ID>-<slug>.md`, frontmatter carries `pillar`/`status`/`priority`); this board indexes
them by status. New work? Copy [`_TEMPLATE.md`](_TEMPLATE.md). For the bigger picture see the
[docs map](../README.md); for the working loop see [AGENTS.md](../../AGENTS.md) → **"Start here"**.

> Keep this board in sync when a story's `status` changes. (A small generator that rebuilds it from
> frontmatter may automate this later — see the docs map.)

## Status
- **Released:** v0.2.4 (2026-06-25). **In flight (`[Unreleased]`):** provider wire robustness —
  OpenRouter/Ollama via the Anthropic Messages protocol + inline tool-call recovery. See
  [CHANGELOG](../../CHANGELOG.md).
- **Gate:** green — `cargo test` · `clippy -D warnings` · `fmt` · the `flux-codegate` layering lint.

## Now (in progress)
_(none)_

## Next (ready — take the top one unless the user named a story)
| # | Story | Pillar | Goal |
|---|---|---|---|
| 1 | [L-01 — Global skills dir loader](L-01-global-skills.md) | Language | load skills from a user/global dir **and** the project dir |

## Blocked
_(none)_

## Backlog (unranked — promote to **Next** with a `priority` when ready)
- [A-01 — Unify SDK onto FlowEngine](A-01-unify-flowengine.md) · Agent · retire the second turn loop
- [C-01 — Crate consolidation, phases 2–4](C-01-crate-consolidation.md) · Core · 33 → ~28–29 crates
- [I-01 — Statistically clean headline gain](I-01-headline-gain.md) · Improve · trials ≥ 3, grader-confirmed

## Done
Completed stories roll into [CHANGELOG.md](../../CHANGELOG.md): set `status: done` in the file and
remove its row here.
