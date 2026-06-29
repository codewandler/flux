# Design: integration plugin pack (in-repo `plugins/`)

**Status:** proposed (story [D-08](../stories/D-08-integration-plugin-pack.md)) · **Layer:** consumes the
redesigned `flux-plugin` (L4, [D-10](process-plugin-protocol.md)) + `flux-datasource`/`flux-secret` (L0) ·
**Home:** an **in-repo nested `plugins/` cargo workspace** (excluded from the root workspace) ·
**Owner:** Timo

## Why

flux ships a production-ready plugin **host** (`flux-plugin`) but **no integration plugins**. The
Slack-channel assistant's "DevOps assistant" scope needs the surface the fluxplane Go bot had: Slack ops, web search,
GitLab, Jira, Confluence, Kubernetes, Loki, Prometheus. Mechanism (decided): **native flux plugins**, not
MCP — each a subprocess speaking flux's protocol. **Prerequisite:** that protocol is redesigned first
([D-10](process-plugin-protocol.md)) so a plugin can contribute datasource records, authenticate by
purpose, and resolve endpoints.

## Home — an in-repo `plugins/` workspace (decision reversed this session)

The original plan put these in a **new sibling `flux-plugins` repo**. **Decided this session: they live
inside the flux repo**, as a nested cargo workspace **excluded from the root workspace** — so the heavy
integration deps (k8s client, cloud SDKs) stay out of the main `flux` gate (`cargo build --workspace`),
yet everything is one repo and one CI. Plugin binaries are subprocesses, so there is **no layering edge**
for `flux-codegate` (which scans only `crates/*`).

```
plugins/                     # its own cargo workspace; not a root member (root: exclude = ["plugins"])
  Cargo.toml                 # path-dep ../crates/{flux-plugin, flux-datasource, flux-secret}
  host-kit/                  # shared helper over D-10's binding SDK: manifest builder, secret-by-purpose
                             # fetch via host callback, dispatch boilerplate, a Record emitter for D-07
  slack/  websearch/  gitlab/  jira/  confluence/  kubernetes/  loki/  prometheus/
                             # one binary crate per integration
```

Each plugin crate is a small `main` that declares a **manifest** (ops, datasources, auth-by-purpose,
endpoints, required host capabilities) and serves the protocol loop. `host-kit` removes the boilerplate so
a new plugin is mostly "declare ops/datasources + implement each against the vendor API."

## Manifest & capability model (over the D-10 protocol)
- A plugin declares its ops (name + input JSON Schema + access/effects/risk/idempotency for the
  authorization floor), its **datasources** (`flux-datasource` `Declaration`s — entities it can
  list/search/contribute), its **auth methods by purpose** + **endpoints**, and the host capabilities it
  needs. The host **denies by default** and authorizes from the manifest — see
  [process-plugin-protocol.md](process-plugin-protocol.md).
- **Secrets** are fetched by purpose via the host protocol (the plugin never reads state files); the host
  resolves a purpose → `flux-secret` material — `env/GITLAB_PERSONAL_TOKEN`, `plugin/slack/main/bot_token`,
  etc. — and can inject it into a host HTTP call (e.g. bearer).
- Ops become policy-gated tools in the consuming agent; the Slack-channel assistant's **D-09 op-grant** list names the
  ops it allows (e.g. `gitlab.*`, `slack.post`), so they run under the headless approver without `--yes`.

## Datasource records (feed D-07) — via the L5 bridge
Where an integration exposes searchable entities (GitLab MRs/issues, Slack channels/users), the plugin
emits **`flux-datasource` `Record`s** via `host-kit`. They reach the D-07 persistent index through a new
**`DatasourceHostCaps`** in `flux-capabilities` (L5): it wraps `flux-plugin`'s `SystemHostCaps` and
services the datasource record/search/get host commands against the index. The bridge lives at L5 (not in
flux-plugin, L4) because the index is L5 — flux-plugin defines only the trait + protocol. The record
contract is identical across local docs and live integrations (the shared `flux-datasource` schema).

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
- The **bridge**: a plugin emits a record → it is retrievable via the datasource `search`/`get` ops
  (proves `DatasourceHostCaps` ↔ the D-07 index round-trip).
- `host-kit`: a unit test that a declared manifest serializes to the protocol shape the host expects.
- The `plugins/` workspace builds/tests on its own (`cargo build/test` inside `plugins/`), kept out of the
  main flux gate.

## Prior art (copy the op/datasource shapes, not the code)
`fluxplane-plugins/{slack,gitlab,jira,confluence,kubernetes,loki,prometheus,websearch}` — the op sets and
datasource entities are a proven inventory; reimplement against flux's redesigned protocol + `host-kit`.

## Non-goals (v1)
- An OpenAPI **dynamic-tool** plugin (the bot indexes OpenAPI specs as RAG docs via D-07 instead).
- A plugin marketplace / `.dex`-style endpoint+grant+index registry (config + env per integration for v1).
- In-process plugins (everything is a subprocess; that is the flux model).
