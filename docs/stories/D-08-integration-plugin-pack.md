---
id: D-08
title: Integration plugin pack — native flux plugins for the DevOps surface
pillar: Agent
status: done
theme: downstream-managed-services
design: docs/designs/integration-plugins.md
---

# Integration plugin pack — native flux plugins for the DevOps surface

## Goal
A reusable set of **native flux plugins** (framed NDJSON over stdio, capability-gated) that give an agent
the integration surface the fluxplane Go bot had: Slack ops, web search, GitLab, Jira, Confluence,
Kubernetes, Loki, Prometheus. Each plugin declares a manifest, exposes **ops** and **datasource records**
(feeding D-07), and is denied-by-default per the host capability model. **Epic** — ship per integration.

## Why (downstream: Slack-channel assistants)
The Slack-channel assistant's "DevOps assistant" scope is exactly this surface. flux has a production-ready plugin
**runtime** (`flux-plugin`) but **zero integration plugins** ship with it. The chosen mechanism is **native
flux plugins** (not MCP). **Home (decided this session — reverses the original sibling-repo plan):** an
**in-repo nested `plugins/` cargo workspace**, *excluded from the root workspace* so heavy integration deps
(k8s client, cloud SDKs) stay out of the main `flux` gate while living in one repo. Each plugin is a
subprocess binary speaking the redesigned protocol (**D-10**).

## flux gap
`flux-plugin` (the host) exists but its v1 protocol can't carry datasource records / auth-by-purpose /
endpoints — **D-10** redesigns it first. There is no shared host-caller/auth helper for plugin authors, no
plugin yet, and no bridge from a plugin's contributed records into D-07's index.

## Acceptance (per slice)
- [ ] Slice 1 — **Slack ops** (post/edit/react/search/users/channels/thread) + **websearch**
      (Tavily + DuckDuckGo): manifests + ops; secrets via `flux-secret` env refs; capability-deny-by-default
      proven by test; one hermetic round-trip per plugin through the runtime. (Unblocks the assistant MVP.)
- [ ] Slice 2 — **GitLab** (projects/MRs/issues/users/groups/CI) ops + datasource records into D-07.
- [ ] Slice 3 — **Jira + Confluence** (issues/projects; pages/spaces).
- [ ] Slice 4 — **Kubernetes** (namespaced inventory, allow-listed).
- [ ] Slice 5 — **Loki + Prometheus** (log queries; PromQL + alerts/targets).
- [ ] A shared **`host-kit`** helper (in `plugins/host-kit`) over D-10's binding SDK: manifest builder,
      secret-by-purpose fetch via the host protocol (never reads state files), NDJSON dispatch, and a
      **`Record` emitter** that contributes `flux-datasource` records.
- [ ] The **plugin↔index bridge**: a `DatasourceHostCaps` (in `flux-capabilities`, L5) that wraps
      `SystemHostCaps` and services the datasource record/search/get host commands against the D-07
      persistent index — so a plugin's contributed records become searchable knowledge. Failing-first test:
      a plugin emits a record → it is retrievable via the datasource `search`/`get` ops.
- [ ] Each slice: full gate green in the `plugins/` workspace; the assistant's **D-09 op-grant** list
      names the ops.

## Progress
- Ready (epic). **Depends on D-10** (the redesigned protocol + binding SDK) and **D-07** (the
  `flux-datasource` schema + index). First action once unblocked: the `plugins/` nested workspace +
  `host-kit` + the L5 bridge, then slice 1.

## Notes
- Home: **in-repo `plugins/` workspace** (excluded from root) — *not* a sibling `flux-plugins` repo
  (decision reversed this session). Path-deps `../crates/flux-plugin`, `../crates/flux-datasource`,
  `../crates/flux-secret`. Subprocess binaries → no `flux-codegate` layering edge (it scans `crates/*`).
- Reuse, don't reimplement: D-10's protocol + binding SDK + capability manifest; `flux-secret` refs
  (`env/…`, `plugin/<name>/<instance>/<key>`). Prior art for the op/datasource shapes (copy shapes, not
  code): `fluxplane-plugins/{slack,gitlab,jira,confluence,kubernetes,loki,prometheus,websearch}`.
- Records contributed here match **D-07**'s `flux-datasource` schema. Serves downstream assistant integration flows. Non-goal
  (v1): an OpenAPI dynamic-tool plugin (the bot indexes OpenAPI as RAG docs via D-07 instead); a plugin
  marketplace/`.dex`-style endpoint registry.
