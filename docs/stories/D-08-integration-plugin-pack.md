---
id: D-08
title: Integration plugin pack — native flux plugins for the DevOps surface
pillar: Agent
status: backlog
priority:
theme: downstream-managed-agents
design: docs/designs/integration-plugins.md
---

# Integration plugin pack — native flux plugins for the DevOps surface

## Goal
A reusable set of **native flux plugins** (framed NDJSON over stdio, capability-gated) that give an agent
the integration surface the fluxplane Go bot had: Slack ops, web search, GitLab, Jira, Confluence,
Kubernetes, Loki, Prometheus. Each plugin declares a manifest, exposes **ops** and **datasource records**
(feeding D-07), and is denied-by-default per the host capability model. **Epic** — ship per integration.

## Why (downstream: Slack-channel assistant)
The Slack-channel assistant's "DevOps assistant" scope is exactly this surface. flux has a production-ready plugin
**runtime** (`flux-plugin`: NDJSON, manifest, host-capability callbacks) but **zero integration plugins**
ship with it. The chosen mechanism is **native flux plugins** (not MCP), housed in a **new sibling
`flux-plugins` repo** (mirroring `~/projects/fluxplane/fluxplane-plugins`) so heavy deps (k8s client, etc.)
stay out of flux's published crate closure. This epic + its roadmap entry are the tracked driver in flux;
the plugin code lives in `flux-plugins`.

## flux gap
`flux-plugin` (the host) exists; the plugins do not. No shared host-caller/auth helper for plugin authors;
no convention for a plugin contributing datasource records into D-07's schema.

## Acceptance (per slice)
- [ ] Slice 1 — **Slack ops** (post/edit/react/search/users/channels/thread) + **websearch**
      (Tavily + DuckDuckGo): manifests + ops; secrets via `flux-secret` env refs; capability-deny-by-default
      proven by test; one hermetic round-trip per plugin through the runtime. (Unblocks the Slack-channel assistant MVP.)
- [ ] Slice 2 — **GitLab** (projects/MRs/issues/users/groups/CI) ops + datasource records into D-07.
- [ ] Slice 3 — **Jira + Confluence** (issues/projects; pages/spaces).
- [ ] Slice 4 — **Kubernetes** (namespaced inventory, allow-listed).
- [ ] Slice 5 — **Loki + Prometheus** (log queries; PromQL + alerts/targets).
- [ ] A shared **host-caller/auth helper** so each plugin fetches secrets via the host protocol (never
      reads state files) and declares its `(process, secret, http)` capabilities.
- [ ] Each slice: full gate green in `flux-plugins`; the Slack-channel assistant's **D-09 op-grant** list names the ops.

## Progress
- Backlog (epic). First action: scaffold the `flux-plugins` repo (mirror `fluxplane-plugins`) + the shared
  helper, then slice 1.

## Notes
- Reuse, don't reimplement: `flux-plugin`'s NDJSON protocol + capability manifest + host-capability
  callbacks; `flux-secret` refs (`env/…`, `plugin/<name>/<instance>/<key>`). Prior art for the op/datasource
  shapes: the `fluxplane-plugins/{slack,gitlab,jira,confluence,kubernetes,loki,prometheus,websearch}` crates.
- Records contributed here must match **D-07**'s datasource record schema (keep that contract
  plugin-friendly). Serves Slack-channel assistant **S-04**. Non-goal (v1): an OpenAPI dynamic-tool plugin (the bot
  indexes OpenAPI as RAG docs via D-07 instead); a plugin marketplace/`.dex`-style endpoint registry.
