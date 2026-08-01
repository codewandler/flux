---
id: C-440
title: "The topologies page — every way to run flux, with pros, cons and the command that does it"
pillar: Core
status: ready
priority: 6
design: docs/designs/remote-agents.md
epic: remote-agents
areas: [website, docs]
note: "public docs, and useful NOW — several topologies already ship (fully local, OS-sandboxed, embedded SDK, served over A2A, portable wasm). ⚠ Every row MUST carry an honest status: a page listing mostly-unbuilt topologies as though they were available is worse than no page"
---

# Every way to run this, and what each costs

## Goal

One public page enumerating the topologies flux can run in — what moves, what stays, what it costs, and
the command that does it.

## Why now, not after remote lands

Most of these **already exist** and nothing collects them, so users discover the shape of the product by
accident. The page is useful the day it ships and gains rows as the remote-agents epic lands.

## The axes, and why they are the right ones

A topology is decided by where four things sit — and they move **independently**, which is exactly what
makes this confusing without a page:

1. **the runtime** (decides: authorization, approval, policy)
2. **the system** (does: IO, spawn, egress)
3. **the model** (local provider vs hosted)
4. **the workspace** (whose files)

`execution-substrate.md`'s rule is what makes the split legible: *"`flux-runtime` decides whether
something may happen. `flux-system` is where it happens."*

## The rows to cover, with today's honest status

| topology | what it is | status |
|---|---|---|
| **Fully local** | everything on your machine — `flux tui` | ships |
| **Local, OS-sandboxed** | effects confined at the single spawn choke point (bubblewrap / Seatbelt) | ships (D-134…D-137); unattended runs default to it since C-410 |
| **Local runtime, containerized ops** | effects in a container | [C-397](C-397-container-process-backend.md), backlog |
| **Local runtime, remote system** | `flux tui --remote <addr>` — approval local, effects remote | this epic; [C-399](C-399-remote-guarded-io-backend.md) ready |
| **Served agent, thin client** | the whole agent runs elsewhere — A2A card, JSON-RPC `message/send`/`message/stream`, sessions | server side **ships** (`flux app run --serve`); the client is the gap |
| **Embedded (SDK)** | another Rust program embeds `flux-sdk` | ships |
| **Portable wasm** | a wasm embedder serves the port through host imports | [C-268](C-268-portable-wasm-runtime-epic.md) epic |
| **Hosted / multi-tenant** | flux-exchange holds credentials, terminates channels, runs ops per tenant | [ecosystem](../designs/ecosystem.md); exchange is v0.9.0 |

⚠ Verify each status against the tree before publishing — this table is a starting set written from the
board, and the board is not the code.

## Acceptance

- [ ] A public page under `website/docs/` covering each topology: **what moves, what stays, pros, cons,
      and a runnable command or config**.
- [ ] ⚠ **Every row carries an honest status** — ships · partial · proposed, with a link. A page that
      lists mostly-unbuilt topologies as if available is worse than no page, and this repo's own norm is
      explicit about this: `vision.md` says the improvement-loop pillar is *"currently aspirational, and
      this document says so honestly."* Match that register.
- [ ] Each row answers **"where are my files"** and **"where does the approval prompt appear"** — the two
      questions a reader actually has, and the ones that differ most between topologies.
- [ ] The guarantees column comes from [C-437](C-437-which-guarantees-travel.md) rather than being
      re-derived. ⚠ Two divergent statements about what flux guarantees is worse than one late one.
- [ ] ⚠ **`ssh` is named as a legitimate option**, not omitted. Running flux on the remote box over `ssh`
      works today and is the right answer for some people; a page that hides it to make the product look
      necessary is not credible about the rest.
- [ ] Commands on the page are pinned by a test, so a CLI change breaks CI rather than breaking the page
      silently — the `website_in_sync` machinery is the precedent.
- [ ] Full gate green, including website checks.

## Notes

- Pairs with the [flux-recipes](../designs/flux-recipes.md) epic's positioning work — same register,
  same rule that a claim needs a command behind it.
- ⚠ The model axis is *orthogonal* and worth one short section rather than doubling the table: a local
  provider (Ollama) versus a hosted one is independent of where the runtime and system sit.
- Do not let this become a marketing page. It is a decision aid; its value is that it says what each
  option **costs**.

## Progress

- Filed 2026-08-01 with the remote-agents epic, at the owner's request for a public enumeration of all
  topologies.
