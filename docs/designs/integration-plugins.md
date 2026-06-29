# Design: integration plugin pack (`flux-plugins`)

**Status:** proposed (story [D-08](../stories/D-08-integration-plugin-pack.md)) · **Layer:** consumes
`flux-plugin` (L4) + `flux-secret` (L0) · **Home:** new sibling repo `~/projects/flux-plugins` ·
**Owner:** Timo

## Why

flux ships a production-ready plugin **host** (`flux-plugin`: framed NDJSON over stdio, a capability
manifest, host-capability callbacks) but **no integration plugins**. The Slack-channel assistant's "DevOps assistant"
scope needs the surface the fluxplane Go bot had: Slack ops, web search, GitLab, Jira, Confluence,
Kubernetes, Loki, Prometheus. Mechanism (decided): **native flux plugins**, not MCP — each a subprocess
speaking flux's protocol. They live in a **sibling repo** (mirroring `~/projects/fluxplane/fluxplane-plugins`)
so heavy deps (k8s client, cloud SDKs) never enter flux's published crate closure.

## Repo shape (`flux-plugins`)

```
flux-plugins/
  Cargo.toml                 # workspace; path-dep flux-plugin, flux-secret from ../flux
  host-kit/                  # shared helper: manifest builder, secret fetch via host callback,
                             # NDJSON op-dispatch boilerplate, a Record emitter for D-07
  slack/  websearch/  gitlab/  jira/  confluence/  kubernetes/  loki/  prometheus/
                             # one binary crate per integration
```

Each plugin crate is a small `main` that: declares a **manifest** (ops, datasources, required
`(process, secret, http)` capabilities), then serves the NDJSON loop dispatching op calls. The `host-kit`
removes the boilerplate so a new plugin is mostly "declare ops + implement each op against the vendor API."

## Manifest & capability model (reuse `flux-plugin`)
- A plugin declares its ops (name + input JSON Schema + effect/subject for the authorization floor) and its
  **datasources** (entities it can list/search), plus the capabilities it needs. The host **denies by
  default** and checks every call — unchanged from `flux-plugin`.
- **Secrets** are fetched via the host protocol callback (the plugin never reads state files): tokens are
  `flux-secret` refs — `env/GITLAB_PERSONAL_TOKEN`, `plugin/slack/main/bot_token`, etc.
- Ops become policy-gated tools in the consuming agent; the Slack-channel assistant's **D-09 op-grant** list names the
  ops it allows (e.g. `gitlab.*`, `slack.post`), so they run under the headless approver without `--yes`.

## Datasource records (feed D-07)
Where an integration exposes searchable entities (GitLab MRs/issues, Slack messages/users/channels), the
plugin emits **D-07 `Record`s** (`entity`, `id`, `source`, `title`, `body`, `links`) via the `host-kit`
emitter, so `search`/`list`/`get`/`relation` work uniformly across local docs and live integrations. Keep
the record contract identical to D-07's schema.

## Slices (ship per integration)
1. **Slack ops** (post/edit/react/search/users/channels/thread) + **websearch** (Tavily + DuckDuckGo
   aggregation) — unblocks the Slack-channel assistant MVP (the bot can *answer*, not just receive).
2. **GitLab** (projects, MRs, issues, users, groups, CI/CD) + datasource records.
3. **Jira + Confluence** (issues/projects; pages/spaces).
4. **Kubernetes** (namespaced inventory, allow-listed namespaces).
5. **Loki + Prometheus** (log queries; PromQL + alerts/targets).

## Testing
- Per plugin: a hermetic **op round-trip** through the `flux-plugin` runtime with a stub vendor client
  (no network) — request → manifest op → typed response; assert **capability-deny-by-default** (an op that
  needs an ungranted capability is refused).
- `host-kit`: a unit test that a declared manifest serializes to the protocol shape the host expects.

## Prior art (copy the op/datasource shapes, not the code)
`fluxplane-plugins/{slack,gitlab,jira,confluence,kubernetes,loki,prometheus,websearch}` — the op sets and
datasource entities are a proven inventory; reimplement against flux's protocol + `host-kit`.

## Non-goals (v1)
- An OpenAPI **dynamic-tool** plugin (the bot indexes OpenAPI specs as RAG docs via D-07 instead).
- A plugin marketplace / `.dex`-style endpoint+grant+index registry (config + env per integration for v1).
- In-process plugins (everything is a subprocess; that is the flux model).
