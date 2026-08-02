---
id: C-440
title: "The topologies page — every way to run flux, with pros, cons and the command that does it"
pillar: Core
status: done
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

- [x] A public page under `website/docs/` covering each topology: **what moves, what stays, pros, cons,
      and a runnable command or config**. → `website/docs/topologies.md`, one section per row; linked
      from the Fundamentals category in `website/sidebars.js`.
- [x] ⚠ **Every row carries an honest status** — ships · partial · proposed, with a link. A page that
      lists mostly-unbuilt topologies as if available is worse than no page, and this repo's own norm is
      explicit about this: `vision.md` says the improvement-loop pillar is *"currently aspirational, and
      this document says so honestly."* Match that register. → every status re-verified against the
      tree, not the board; three rows were corrected (see `## Progress`). Pinned by
      `topologies_page_states_a_status_for_every_row_and_names_ssh`.
- [x] Each row answers **"where are my files"** and **"where does the approval prompt appear"** — the two
      questions a reader actually has, and the ones that differ most between topologies. → both are
      columns of the at-a-glance table (pinned) and a bullet in every section.
- [ ] The guarantees column comes from [C-437](C-437-which-guarantees-travel.md) rather than being
      re-derived. ⚠ Two divergent statements about what flux guarantees is worse than one late one.
      → **not done, deliberately.** C-437 is still `ready`, so there is nothing to source from. Rather
      than re-derive a guarantees column and create the second divergent statement this item exists to
      prevent, the page carries no such column; the remote-system section states the open question and
      defers. C-437 adds the column.
- [x] ⚠ **`ssh` is named as a legitimate option**, not omitted. Running flux on the remote box over `ssh`
      works today and is the right answer for some people; a page that hides it to make the product look
      necessary is not credible about the rest. → a full row and section of its own, and the
      remote-system section points at it as the bar to beat. Pinned by the same test.
- [x] Commands on the page are pinned by a test, so a CLI change breaks CI rather than breaking the page
      silently — the `website_in_sync` machinery is the precedent. → `website_contract.rs` was the
      closer precedent (it is the executable contract for *hand-maintained* website pages;
      `website_in_sync` mirrors generated blocks). Three tests added there.
- [x] Full gate green, including website checks.

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
- Shipped as `website/docs/topologies.md` (Fundamentals category), with nine rows — the eight from the
  table above plus `ssh` as a **peer row**, not a callout: the acceptance requires it be named as
  legitimate, and a callout under another section reads as a caveat where a row reads as an option.
- Three pins in `crates/flux-cli/tests/website_contract.rs`, not `website_in_sync.rs`. The latter
  mirrors *generated* blocks into the site; this page is hand-written, and `website_contract.rs` is
  already the executable contract for exactly that class. The pins:
  `topologies_page_runnable_commands_are_real_cli_surface` resolves every `flux …` line in an `sh`
  fence against the shipped binary's own `--help`, walking the subcommand tree and matching each long
  flag on a word boundary; `topologies_page_does_not_present_unbuilt_surface_as_runnable` fails **when
  `flux tui --remote` starts to exist**, so the row's status is updated by the change that makes it
  stale rather than by someone noticing later; `topologies_page_states_a_status_for_every_row_and_names_ssh`
  requires a status word on every table row, both reader-questions as columns, and `ssh` on the page.
- Fence convention the page and the tests share: an `sh` fence runs **today**; a proposed spelling goes
  in a `text` fence. Neither a reader nor the test can confuse the two.
- ⚠ **Statuses were re-verified against the tree, and the starting table above was wrong in four
  places.** Recorded here because the corrections outlive this story:
  - **"the client is the gap" is false.** `flux a2a <url>` ships (`crates/flux-cli/src/a2a_cmd.rs`,
    `crates/flux-a2a/src/client.rs`). The real gap is **approval**: no approver in the tree speaks over
    a network — `StdinApprover`, `ChannelApprover` and `SubAgentApprover` are all local, and
    `crates/flux-app/src/app.rs:661-665` is binary (`auto_approve` → `AllowApprover`, else
    `DenyApprover`). A served agent can only allow everything or deny everything. That is the
    served-agent row's headline caveat.
  - **flux-exchange is v0.11.0**, not v0.9.0, and it does not terminate channels (not built). A minted
    agent token authenticates nothing yet, and a multi-tenant deployment *refuses* process, container,
    socket and plugin runtimes — it serves HTTP and remote only. Row marked **partial**.
    ⚠ `docs/ecosystem.md:127-131` is stale in the same direction ("binds no port, holds no credential,
    and answers no request" — true at v0.4.0, false now). Deliberately **not** touched here; filed
    separately. The page does not inherit the claim.
  - **Portable wasm** is the Flux-Lang evaluation core only, with zero host authority (`NoAuthority`,
    empty catalogue, every dispatch denied). Row marked **partial**, not "epic".
  - **The OS sandbox** needed a caveat the table omitted: the *interactive* default is `Off`; only
    unattended surfaces default to `Require`. Windows has no backend, so unattended refuses to start
    and interactive runs unconfined.
- ⚠ **Acceptance item 4 (the guarantees column from C-437) is deliberately deferred, not forgotten.**
  C-437 is still `ready`, so there is nothing to source from, and re-deriving a guarantees column here
  would manufacture exactly the second divergent statement that item exists to prevent. The page
  therefore carries no guarantees column; the remote-system section states the open question and
  defers. **C-437 adds the column** — that story, not a follow-up.
