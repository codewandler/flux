---
id: L-03
title: Native-text module declarations (the whole app in flux-lang — `.flux` does all of it)
pillar: Language
status: done
priority:
theme: downstream-managed-agents
design: ../designs/native-text-modules.md
---

# Native-text module declarations

## Goal
Let a `.flux` file declare the **whole app** — agents, channels, datasources, triggers, journeys, and the
behavioral loops (flows) — in flux-lang's **native text**, instead of smuggling JSON for the program
layer. Channels / datasources / auth are configured as **typed module declarations** directly in
flux-lang; `.flux` means exactly one language.

## Why (motivating consumer: Slack-channel assistant)
The downstream **Slack-channel assistant** (second downstream flux consumer) is a declarative app — a `slack` channel
+ an agent-bound trigger + an `assistant` agent + a docs datasource. It should be the human-authored,
version-controlled artifact native flux-lang is for, not JSON. The whole ecosystem (managed-agents) already
authors flows as native text via `flux_lang::parse::parse`; this makes the app layer match.

## Acceptance
- [x] A native-text grammar for the program layer: `agent` / `channel` / `datasource` / `trigger` /
      `journey` declarations (name + indented `key value` settings; the trigger `on`/`run`/`agent`; a
      journey's inline `flow` body in the existing flow syntax). Settings are flux-lang values
      (strings/numbers/bools/lists/records/bare-idents). **Test:** `parse_program_reads_the_full_typed_module_surface`.
- [x] Secrets as **references, never inline**: `secret "ENV_NAME"` → a `{"$secret":…}` marker in the
      pure parser, resolved from the environment at load by the host (`flux_app::resolve_secrets`); a
      missing var errors naming the var, not the value.
- [x] `Module::parse_str` parses native text (bare `flow` → `Module::Flow`; module decls → `Program`);
      the **JSON-program path is deleted** (clean cutover — `from_json`/`PROGRAM_KEYS` gone). `flux flow run`
      sniffs JSON-vs-text so native-text loops load too.
- [x] `datasource` declarations drive `flux app run`'s knowledge backend (`build_datasources`,
      markdown/openapi ingesters), replacing the implicit workspace auto-index on the app path.
- [x] Examples rewritten to native text (`crates/flux-app/examples/{hello,support-bot}.flux`,
      `examples/channels-app.flux`); `flux app run …` loads them unchanged. Full gate green;
      `flux-codegate` layer placement unchanged (no new crates).

## Progress
- **Done.** Shipped as native-text typed-module declarations (decision: typed decls, not a generic
  `module <kind>` registry; whole surface at once; secrets ref-only). Supersedes the original
  "native-text *program* grammar (text→Program parse, JSON kept additively)" framing — the JSON program
  path was instead deleted (clean cutover), and the surface was widened to datasources + auth/secrets.
  Design: [native-text-modules.md](../designs/native-text-modules.md).

## Notes
- No new node kinds — module declarations are pure data; the flow language is unchanged.
- Pairs with the shipped native value-template syntax (records/lists) for settings bags.
- Sibling-pillar context: [[L-01]] global skills, L-02 flux-markdown. Downstream theme: the Slack-channel assistant
  integration stack (D-07/D-08/D-09/D-10, all shipped).
