---
id: L-03
title: Native-text program/app grammar (channels/triggers/agents/journeys in flux-lang)
pillar: Language
status: backlog
priority:
theme: downstream-managed-agents
design:
---

# Native-text program/app grammar

## Goal
Let a `.flux` **program** (the multi-agent app layer — `agents` / `channels` / `triggers` / `journeys`) be
authored in flux-lang's **native text** syntax, the way flows and types already are — instead of JSON. Today
`.flux` means flux-lang for flows (`crates/flux-lang/examples/call-routing.flux`,
`crates/flux-flow/assets/agent-loop.flux`) but **smuggles JSON for apps** (`crates/flux-app/examples/hello.flux`,
`examples/channels-app.flux`), which is an inconsistency in the language surface.

## Why (motivating consumer: Slack-channel assistant)
The downstream **Slack-channel assistant rewrite** (second downstream flux consumer) is a declarative program —
`bot.flux` = a `slack` channel + an `agent`-bound trigger + an `assistant` agent. It currently **must be
written as JSON** because the program layer has no native grammar, even though the bot is exactly the kind
of human-authored, version-controlled artifact native flux-lang is for. `bot.flux` is kept as JSON until
this lands (decision recorded in the Slack-channel assistant repo).

## flux gap (grounded in the shipped code)
- Native text parses **flows + types** only. Grepping the flux-lang parser for `channel`/`trigger`/`journey`
  keywords returns nothing.
- The program layer is **JSON-only**: `flux_lang::program::Module::parse_str` is literally
  `serde_json::from_str(...)` → `Module::from_json` (`crates/flux-lang/src/program.rs`), and `flux app run`
  loads via that path. The decls (`Program`/`AgentDecl`/`ChannelDecl`/`TriggerDecl`/`JourneyDecl`) are
  pure-data structs with no surface grammar.

## Acceptance
- [ ] A native-text grammar for the program layer: `agent` / `channel` / `trigger` / `journey` declarations
      (name + `settings` as an inline record, the trigger `on`/`run`/`agent`, and a journey's `flow` body in
      the existing native flow syntax). Settings bags (e.g. Slack tokens, `settings.system_prompt`) use the
      already-shipped native value-template records (`{ k: expr }`). **Failing-first test:** parse a
      native-text program (a `slack` channel + an agent-bound trigger + an `assistant` agent) and assert it
      equals the equivalent JSON-parsed `Program`.
- [ ] `Module::parse_str` accepts native text (sniff text-vs-JSON, or a unified front end) **and still loads
      existing JSON programs** (additive — `hello.flux` / `channels-app.flux` keep working).
- [ ] The Slack-channel assistant `bot.flux` is rewritten from JSON into native flux-lang and `flux app run bot.flux`
      loads it unchanged (cross-repo consumer validation).
- [ ] Full gate green; `flux-codegate` layer placement unchanged.

## Progress
- Backlog. Surfaced by the Slack-channel assistant rewrite (`bot.flux` authored as JSON, 2026-06-29) — the program/app
  layer is the last part of `.flux` with no native-text form.

## Notes
- Pairs with the shipped native value-template syntax (records/lists) for the `settings` bags. Consider a
  one-way text→`Program` parse first (pretty-printing back to text is optional).
- Relevant files: `crates/flux-lang/src/program.rs` (the decls + `Module`), the existing native-text flow
  parser/lexer, and the `flux app run` load path (`crates/flux-cli/src/main.rs` `run_app`).
- Sibling-pillar context: [[L-01]] global skills, L-02 flux-markdown. Downstream theme: the Slack-channel assistant
  integration stack (D-07/D-08/D-09/D-10, all shipped).
