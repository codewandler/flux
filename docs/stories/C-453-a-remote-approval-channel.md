---
id: C-453
title: "No approver in the tree speaks over a network — a served agent can only allow everything or deny everything"
pillar: Core
status: ready
priority: 2
design: docs/designs/remote-agents.md
epic: remote-agents
areas: [flux-runtime, flux-server]
note: "⚠ a live hole in a SHIPPED surface, not a design gap. Every approver is local — StdinApprover, ChannelApprover (in-process), SubAgentApprover — and grep for approval across flux-server and flux-a2a returns nothing. flux-app/src/app.rs:661-665 is binary. It is also the prerequisite for every hosted topology"
---

# The envelope's middle stage has no remote form

## Goal

A human can approve or refuse **one specific effect** over a network, through the same
`flux_runtime::Approver` contract the local surfaces use.

## ⚠ The finding, verified

flux's headline claim is that every effect passes **authorization → approval → guarded IO**. On the
shipped remote surface the middle stage does not exist:

- Every real approver is local: `StdinApprover` (`crates/flux-cli/src/session.rs:1177`),
  `ChannelApprover` (`crates/flux-tui/src/controller.rs:395` — an *in-process* channel),
  `SubAgentApprover` (`crates/flux-orchestrate/src/lib.rs:60`). The rest are the `AllowApprover` /
  `DenyApprover` constants.
- `grep` for approval across `crates/flux-server` and `crates/flux-a2a`: **nothing**.
- `crates/flux-app/src/app.rs:661-665` — `auto_approve` → `AllowApprover`, else `DenyApprover`.
  **Allow everything, or deny everything.**

So `flux app run --serve` is not "the envelope with a remote UI"; it is the envelope with its human
stage removed. C-440's implementor hit the same wall independently while writing the topologies page.

⚠ `docs/designs/ecosystem.md:122-127` anticipated this as the reason not to fuse runtime and system:
*"fusing them would force every consumer of the substrate to also take flux's approval model — including
consumers with no human at a terminal to prompt."* This story builds the terminal.

## Acceptance

- [ ] **Failing-first**: a test asserting a served agent can have one specific effect approved by a
      remote decision, and that an unapproved effect is refused — failing at the merge base.
- [ ] Implemented as an `Approver`, reusing the shape of `ChannelApprover` — it already decouples the
      decision from the terminal, so a network approver is that with a different transport. ⚠ **Do not
      add a second approval concept**; the envelope has one stage and it should keep one.
- [ ] ⚠ **Fails closed when nobody answers.** A timeout must deny, never allow. An approval channel that
      allows on silence is worse than `AllowApprover`, because it looks like a control.
- [ ] ⚠ **An approval is bound to the effect it was granted for** and cannot be replayed onto another.
      The obvious implementation — "the client said yes" — is a confused-deputy waiting to happen.
- [ ] `auto_approve` remains an explicit, visible choice. Closing this hole must not remove the
      deliberate escape hatch.
- [ ] ⚠ The docs state what shipped **before** this — allow-all or deny-all — so it reads as closing a
      hole rather than adding a feature. An operator running a served agent today should learn that they
      have been in one of those two modes.
- [ ] Full gate green.

## Notes

- ⚠ Prerequisite for [C-454](C-454-the-hosted-runtime-locality.md) and for anything that hosts a flux
  runtime. A hosted runtime without this **is** the allow-all configuration the epic exists to avoid.
- Related from the other direction: [C-444](C-444-sdk-secure-defaults.md) — `auto_approve(true)` does not
  imply confinement for SDK embedders. Same class: a documented caveat standing in for a default.
- Anthropic's Managed Agents resolved this by **not having** per-effect approval at all — the caller
  steers or interrupts instead. Coherent for their product; the opposite of flux's thesis. Worth reading
  before designing, precisely because it is the road not taken.

## Progress
- Filed 2026-08-02 while comparing flux's remote-agents model to Anthropic's Managed Agents.
