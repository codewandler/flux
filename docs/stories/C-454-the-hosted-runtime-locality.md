---
id: C-454
title: "The hosted-runtime locality — flux-exchange binds flux's runtime, and you hold a client"
pillar: Core
status: blocked
design: docs/designs/remote-agents.md
epic: remote-agents
areas: [flux-runtime, docs]
note: "⚠ the charter SANCTIONS this and I first read it as forbidding it. `Shipping an interpreter` is the non-goal; `where flux already answers a question, we bind rather than restate` is the rule. Storing flux-app Programs while never shipping an interpreter means exactly one thing — bind flux's own runtime. Blocked on C-453 and on multi-tenant isolation"
---

# Bring the runtime, do not rebuild it

## Goal

A third locality: flux-exchange binds flux's runtime and runs a tenant's stored program; the operator
holds a client.

## What the charter actually says

`docs/designs/ecosystem.md` reads as prohibiting this and does not. It prohibits a **rival execution
model**:

- Non-goal: *"**Shipping an interpreter.**"*
- Non-goal: *"**Reimplementing the engine.** Execution primitives come from `flux-system`… **Where flux
  already answers a question, we bind rather than restate.**"*
- And it already stores them: *"A workflow is a stored, versioned, per-tenant `flux-app` Program."*

⚠ Storing `flux-app` Programs while never shipping an interpreter leaves exactly one option: **bind
flux's own runtime.** That is this story.

## ⚠ What blocks it, and neither is in the exchange

1. **[C-453](C-453-a-remote-approval-channel.md)** — no approver speaks over a network. A hosted runtime
   without one is `AllowApprover` with extra steps, which is the configuration this epic exists to avoid.
2. **Multi-tenant isolation.** `ecosystem.md:107-113`: *"A locally-executing runtime cannot be safely
   multi-tenant in one process."* Anthropic answered the same problem with a sandbox per session. flux's
   answers are [C-397](C-397-container-process-backend.md) (container backend) and
   [C-399](C-399-remote-guarded-io-backend.md) (remote port) — so the prerequisites are **in flux**, not
   in the exchange.

## Acceptance

- [ ] The locality is designed, not built: what the exchange binds, what the client holds, and where the
      approval prompt appears.
- [ ] ⚠ **`flux must never require flux-exchange`** survives, and so does C-399's local-first principle —
      *"flux must be able to do this locally as dev without depending on a service."*
- [ ] ⚠ **No second execution model.** If the design needs anything the flux engine does not already do,
      that is a change to flux, not a new thing in the exchange.
- [ ] The exchange-side reality is recorded so nobody plans around charter: at v0.11.0 **stored workflows
      and execution records are explicitly not built**, minted agent tokens **authenticate nothing yet**,
      and a multi-tenant deployment **refuses** `process`/`container`/`socket` runtimes.
- [ ] `ecosystem.md`'s invariant is honoured: *"the runtime is declared by the connector, never chosen by
      the caller. A caller who can pick the runtime is a caller who can pick an effect."*

## Notes

- Distinct from the **served agent** topology (`flux app run --serve`), which runs on *your* box and has
  the same approval hole — see C-453.
- Distinct from **remote system** ([C-436](C-436-flux-tui-remote.md)), which keeps the brain and the
  approver local on purpose.
- ⚠ Do not start this before C-453. The whole difference between this and "an agent that does whatever
  it likes on someone else's infrastructure" is the approval channel.

## Progress
- Filed 2026-08-02 from the Anthropic Managed Agents comparison.
