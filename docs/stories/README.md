# flux — backlog & status board

The single screen for **"what to work on next and where we are."** One file per story lives in this
directory (`<ID>-<slug>.md`, frontmatter carries `pillar`/`status`/`priority`); this board indexes
them by status. New work? Copy [`_TEMPLATE.md`](_TEMPLATE.md). For the bigger picture see the
[docs map](../README.md); for the working loop see [AGENTS.md](../../AGENTS.md) → **"Start here"**.

> Keep this board in sync when a story's `status` changes. (A small generator that rebuilds it from
> frontmatter may automate this later — see the docs map.)

## Status
- **Released:** v0.2.4 (2026-06-25). **In flight (`[Unreleased]`):** one agent loop everywhere
  (classic `flux-agent::Agent` retired) with per-turn token usage flowing through it; global
  multi-format skills; A2A client; provider wire robustness (OpenRouter/Ollama via the Anthropic
  Messages protocol); and the self-improvement offline half — partial-credit tag scalar, durable token
  capture, and a stable-baseline synthetic loop. See [CHANGELOG](../../CHANGELOG.md).
- **Gate:** green — `cargo test` · `clippy -D warnings` · `fmt` · the `flux-codegate` layering lint.

## Now (in progress)
- [I-01 — Statistically clean headline gain](I-01-headline-gain.md) · Improve · offline half done
  (partial-credit scalar + durable token capture + synthetic `trials = 5` loop); the trials ≥ 5
  grader-confirmed run is **staged** on a funded provider key

## Next (ready — take the top one unless the user named a story)
_(none ready — promote one from Backlog below)_

## Blocked
_(none)_

## Backlog (unranked — promote to **Next** with a `priority` when ready)
- [L-02 — flux-markdown engine + progressive-disclosure skills](L-02-flux-markdown-engine.md) · Language · AST parser, body-on-demand activation
- [C-01 — Crate consolidation, phases 2–4](C-01-crate-consolidation.md) · Core · 33 → ~28–29 crates

## Done
- [A-02 — A2A client (`flux a2a <URL>`)](A-02-a2a-client.md) · Agent · consume a remote A2A agent like a local one; server clean-cutover to the current spec (see [CHANGELOG](../../CHANGELOG.md))
- [A-01 — Unify on FlowEngine, retire the classic Agent loop](A-01-unify-flowengine.md) · Agent · one loop everywhere; `flux-agent` repurposed as the `AgentSpec` home (see [CHANGELOG](../../CHANGELOG.md))
- [L-01 — Global, multi-format skill loading](L-01-global-skills.md) · Language · multi-dir + Agent-Skills/Claude format + `flux-markdown` (see [CHANGELOG](../../CHANGELOG.md))

## Done
Completed stories roll into [CHANGELOG.md](../../CHANGELOG.md): set `status: done` in the file and
remove its row here.
