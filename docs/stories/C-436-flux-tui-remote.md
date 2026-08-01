---
id: C-436
title: "`flux tui --remote <addr>` — the agent runs here, the effects land there"
pillar: Core
status: backlog
design: docs/designs/remote-agents.md
epic: remote-agents
areas: [flux-cli, flux-system, flux-tui]
note: "the surface. ⚠ Blocked on C-399 (remote guarded-IO port, ready) and shaped by C-438 (where the files live) — building the link before the workspace question is answered is how this kind of tool dies. ⚠ The TUI must make remoteness unmissable: an operator who forgets which machine they are on approves the wrong thing"
---

# The agent you drive is here; the system it touches is there

## Goal

`flux tui --remote <addr>` runs an agent locally — your terminal, your approval prompt, your model —
whose effects land on a remote substrate.

## The boundary it rides on

`execution-substrate.md` states the rule this epic puts across a network:

> **`flux-runtime` decides whether something may happen. `flux-system` is where it happens.**

The deciding half stays with you; the happening half moves. `port.rs` already names the case: the port
exists so *"a WebAssembly embedder…, **a remote executor**, or a test double"* can serve the same
guarded operations, and the traits are unsealed.

## Acceptance

- [ ] **Failing-first**: a test asserting an op dispatched under `--remote` executes on the remote
      substrate and not locally — failing at the merge base.
- [ ] Approval, model choice and credentials stay **local**. ⚠ If any of them move, this has become the
      other topology (a served agent), and the story has changed rather than progressed.
- [ ] ⚠ **Remoteness is unmissable in the UI** — persistent, not a startup line that scrolls away. The
      failure this prevents is an operator approving a destructive op believing it lands on a scratch
      box when it lands on their laptop, or the reverse.
- [ ] A dropped link mid-turn is handled and *named*: what happened to the in-flight op, and what the
      run's state is now. ⚠ Do not let "unreachable" and "refused" collapse — C-399's own acceptance,
      and over a network it stops being a nicety since an operator responds to them in opposite ways.
- [ ] `--remote` is opt-in and absent means exactly today's behaviour. Nothing makes a remote required.
- [ ] Full gate green.

## Notes

- **Depends on [C-399](C-399-remote-guarded-io-backend.md)** (the remote guarded-IO port, `ready`) and
  is shaped by [C-438](C-438-where-do-the-files-live.md). ⚠ Build the link before the workspace question
  is answered and you get a tool that demos well and cannot be used for coding.
- ⚠ Not to be confused with serving a whole agent — `flux app run --serve` already exposes an agent over
  A2A (agent card, JSON-RPC `message/send`/`message/stream`, sessions). That moves approval off your
  machine, which is the opposite of this story's purpose. See [C-440](C-440-the-topologies-page.md).
- The CLI rule is explicit subcommands, no implicit default-run; `--remote` is a flag on an existing
  verb, which fits — but check whether `flux run` and `flux app run` want it too, and say so rather
  than shipping it on one verb by accident.

## Progress

- Filed 2026-08-01 with the remote-agents epic.
