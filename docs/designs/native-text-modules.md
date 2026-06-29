# Native-text module declarations — `.flux` does all of it

**Status:** implemented · **Story:** [L-03](../stories/L-03-native-text-program-grammar.md)

## Problem

`.flux` was overloaded. The flux-lang **text** format owned flows (`crates/flux-lang/examples/call-routing.flux`),
but the multi-agent **program/app** layer was smuggled in as **JSON** in the same extension
(`crates/flux-app/examples/hello.flux`, `examples/channels-app.flux`), loaded by a `Module::parse_str`
that was literally `serde_json::from_str`. So one extension meant two unrelated languages.

## Decision

Make `.flux` do all of it, in native flux-lang. A single `.flux` file declares the whole app — agents,
channels, datasources, triggers, journeys, and the behavioral loops (flows) — as **typed module
declarations** configured directly in flux-lang. The declarations stay **pure data** (L0); the existing
L6 hosts (`flux-channels`, `flux-capabilities`, `flux-app`) give them runtime meaning. **No new node
kinds** — the program layer is declarations around the unchanged flow language.

The JSON-program authoring/loading path is **deleted** (clean cutover): `Module::from_json` and the
`PROGRAM_KEYS` sniff are gone; `Module::parse_str` now parses native text. (Bare-flow JSON `DraftAst`
files — the original AST form — still load via `flux flow run`, which sniffs a leading `{`.)

## Grammar

Each declaration is `<keyword> <name>` at column 0, followed by an indented block of `key value`
attribute lines (uniform across decls, so the recursive-descent parser stays simple). A file with only a
`flow` header (no module keywords) still loads as a bare `Module::Flow`.

```
# app.flux — the whole app in native flux-lang
agent assistant
  model "claude-sonnet-4-6"
  tools [search, send]          # bare identifiers in a list coerce to strings
  datasources [docs]
  description "answers from the docs"

channel slack                   # `kind` defaults to the decl name when omitted
  bot_token secret "SLACK_BOT_TOKEN"
  app_token secret "SLACK_APP_TOKEN"

datasource docs
  kind "markdown"
  path "./docs"

trigger on_msg
  on "slack"
  run greet
  agent assistant

journey greet
  agent assistant
  flow                          # an inline flow block — the existing flow-statement parser
    $r = complete($text)
    return $r
```

- **Recognized attributes** map to typed fields: `model`/`tools`/`datasources`/`description` (agent),
  `kind`/`path` (datasource), `on`/`run`/`agent` (trigger), `agent` + inline `flow` (journey). Any
  **unrecognized** `key value` on an agent/channel/datasource accumulates into that decl's `settings`
  bag.
- **Setting values** are evaluated to JSON with no IO: strings / numbers / `true|false|null`, `[ … ]`
  lists, `{ k: v }` records, and bare identifiers (→ strings). `channel`/`datasource` default `kind` to
  the decl name.
- A top-level `flow <name>` is a named flow in `Program.flows`; a trigger's `run` resolves to a journey
  *or* a top-level flow by name (`Program::flow_named`).

## Secrets — one mechanism

`secret "NAME"` lowers (in the **pure** parser) to the reserved marker `{"$secret":"NAME"}` — never
inline plaintext. The marker shape is owned by L0 (`flux_lang::program::{SECRET_KEY, secret_marker,
as_secret_ref}`) so the parser, the host resolver, and consumers all agree on it. The host
(`flux_app::resolve_secrets`, called by `flux app run`) walks the agent/channel/datasource settings
**once** at load and replaces each marker with `std::env::var("NAME")`. A missing variable is a clean
startup error that **names the variable but never any value**; resolved secrets live only in memory and
are never logged. (Future: extend the source to the `flux-auth`/`flux-credentials` keychain.)

This is the **single** secret mechanism: the channel adapters' former string convention
(`"secret:env/KEY"` / `"env:KEY"` resolved per-adapter) was removed — their token fields now read the
already-resolved value. Because the marker is a JSON *object* but token fields deserialize as strings,
resolution **must** run before settings are consumed; `flux_channels::build_channels` therefore guards
each decl and errors clearly if an unresolved marker reaches it (rather than failing with an opaque
serde "expected a string"), so the ordering contract can't be skipped silently.

## Code map

- `crates/flux-lang/src/parse.rs` — `parse_program` (the typed-decl front end), the per-decl parsers, and
  `parse_setting_value` (literals/lists/records/`secret`). Reuses the existing flow header + statement
  parsers for journey/flow bodies.
- `crates/flux-lang/src/program.rs` — `DatasourceDecl` + `Program.datasources`; `Module::parse_str` now
  delegates to `parse_program`; the JSON branch is removed.
- `crates/flux-app/src/secrets.rs` — `resolve_secrets(&mut Program)`.
- `crates/flux-cli/src/main.rs` — `run_app` parses native text → `resolve_secrets` →
  `build_datasources` (from the `datasource` decls; markdown/openapi ingesters) → `build_channels` →
  `App`. `run_flow` sniffs JSON-vs-text so `flux flow run` loads native-text loops too.
- Examples: `crates/flux-app/examples/{hello,support-bot}.flux`, `examples/channels-app.flux`.

## Non-goals

A generic/pluggable `module <kind>` registry (typed decls only); plugin-contributed module kinds; a
keychain secret source (env only this cut); pretty-printing `Program → text` (one-way text→Program parse).
