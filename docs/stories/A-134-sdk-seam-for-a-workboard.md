---
id: A-134
title: "No SDK seam for a `WorkBoard` — decide whether boards are embeddable, then ship `ClientBuilder::try_with_work_board` if they are"
pillar: Agent
status: backlog
epic: fleet-coordinator
design: docs/designs/fleet-coordinator.md
areas: [flux-sdk, flux-capabilities]
note: "filed from A-131's implementor report — live datasources have a documented SDK registration seam, boards have none, so an embedder cannot bind a board at all"
---

# No SDK seam for a `WorkBoard` — decide whether boards are embeddable, then ship `ClientBuilder::try_with_work_board` if they are

## Goal
A live datasource can be bound by an embedder. `ClientBuilder::try_with_live_datasource`
(`crates/flux-sdk/src/lib.rs:549`) installs the generated `<domain>.list` / `<domain>.get` pair, the
domain's evidence group and the configured-domain ambient signal as one unit, and the website
documents that as the supported path — "SDK registration with
`ClientBuilder::try_with_live_datasource` installs the two operations, their domain group, and a
configured-domain ambient signal together" (`website/docs/agent/datasources.md:169`).

A `WorkBoard` has no such seam. The registration function exists one layer down —
`flux_capabilities::try_register_work_board` (`crates/flux-capabilities/src/datasource/board.rs:172`),
generating the six ops in `OPERATIONS` (`board.rs:135`) — but **nothing in `crates/flux-sdk` mentions
`WorkBoard` or `work_board` at all**. So an embedder building an agent through the SDK cannot bind a
board, by any route: not a `MemoryBoard`, not A-114's markdown board, not a custom implementation of
the port.

First question, and the one this story is really for: **are boards meant to be embeddable?** A-131
is making them bindable from a Program declaration, which may be the whole intended surface — a board
is a coordinator's system of record, and one could argue it belongs to a running `flux` rather than to
a library consumer. But live datasources took the opposite position, and the epic's own dogfooding
story runs boards from tests. Decide it deliberately; if the answer is yes, deliver the seam with the
same all-in-one guarantees the live-datasource seam gives, and if the answer is no, say so where an
embedder will look.

## Acceptance
- [ ] The decision is recorded: boards are embeddable through the SDK, or they are deliberately
      Program-only. If Program-only, `website/docs/agent/datasources.md` says it explicitly next to
      the live-datasource seam it documents, and this story closes there — an absent seam that is
      *documented as absent* is a defensible answer; an absent seam that reads as an oversight is not.
- [ ] If embeddable: `ClientBuilder::try_with_work_board` (or whatever name is chosen) binds a
      `Arc<dyn WorkBoard>` under a domain and installs the generated ops. **Failing-first test**: an
      SDK-built client resolves `<domain>.claim` against a `MemoryBoard`; it cannot today, because no
      such builder method exists.
- [ ] The all-in-one property that `try_with_live_datasource` deliberately protects is matched, not
      re-derived: ops, group and ambient signal install together, and calling `groups` /
      `ambient_signals` before or after must not tear the surface apart (`flux-sdk/src/lib.rs:545-548`
      states this as the reason that method retains them separately). Failing-first test for the
      interleaving, mirroring the live-datasource one.
- [ ] Collision behaviour matches the existing seam: a board domain colliding with built-ins or another
      consumer pack is a **source-labelled build error** at composition, not a silent shadow.
      `try_register_work_board` already rejects a bad or duplicate domain
      (`crates/flux-capabilities/tests/work_board_operations.rs` covers the reject cases) — the SDK
      path must surface that, not swallow it.
- [ ] Evidence gating is honest: the board's ops are surfaced only when a board is actually bound,
      the same rule the live-datasource docs state for `support.list` / `support.get`.
- [ ] Documented where an embedder looks: `website/docs/agent/datasources.md` gains the board seam
      beside the live one, and the SDK re-export sweep is satisfied (a public seam that is not
      re-exported is not a seam).
- [ ] Standard gate green in both workspaces (root + `plugins/`), `cargo fmt --check` included.

## Progress
- (not started)

## Notes
- Filed 2026-07-29 from the fleet-coordinator integration run, out of **A-131's implementor report**.
  The evidence as given: `website/docs/agent/datasources.md` documents live-datasource SDK
  registration via `ClientBuilder::try_with_live_datasource`, but `crates/flux-sdk` has no
  `try_with_work_board` equivalent, so boards cannot be bound by an embedder. Re-verified against
  `main` at base `9721daca` — `grep -rn 'work_board\|WorkBoard' crates/flux-sdk/` returns nothing.
- ⚠ Sequence after A-130. That story is deciding whether the dispatch write-back is a **seventh**
  board op or an extension of `claim`, which changes `OPERATIONS: [&str; 6]` (`board.rs:135`) and
  therefore the exact op set any SDK seam installs. Landing this first means absorbing that change.
- Coordinate with A-131, which is choosing the Program-side `kind` name for a board. If both surfaces
  ship, they should name the same concept the same way; a Program calling the board `board` while the
  SDK calls it something else is a documentation problem waiting to happen.
- A-135 is relevant if the seam is delivered: the SDK is where a fleet journey would be tested from,
  and that test currently needs a real loopback listener.
