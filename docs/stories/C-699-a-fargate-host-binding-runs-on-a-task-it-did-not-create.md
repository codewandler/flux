---
id: C-699
title: "A Fargate host binding runs on a task it did not create"
pillar: "Core"
status: backlog
epic: first-class-hosts
areas: [flux-cli, flux-system]
design: first-class-hosts
note: "gated on the isolation-provisioner decision: a Fargate task is created to run something, which is the one thing every host binding is forbidden to do"
---

# A Fargate host binding runs on a task it did not create

## Goal

Fargate is where the host model meets the boundary it has drawn from the start. Every binding so
far consumes a substrate that already exists — a machine, a pod, a guest, an instance — and Decision
0018, C-480 and C-677 all state plainly that Flux never creates one. But a Fargate task exists *in
order to* run something: the useful shape is one task per wave, or per story, created on demand and
gone afterwards. A binding that can only attach to a task somebody else launched is a thin slice of
what an operator wants, and a binding that launches one is the provisioning contract nobody has
written.

This story therefore has two halves, and the first is deliberately small. **Attach**: a `fargate`
binding reaches a task that is already running the serving daemon, addressed by task ARN or by
service, with its credential from Exchange (C-697) — the EC2 shape (C-698) with a different
identity. **Create**: an ephemeral task launched per selection, used, and reliably torn down, is
gated on a decision defining provision/attach/teardown as a granted, audited operation family with
a guaranteed-teardown obligation — because an orphaned task is a running cost and a live credential
holder, not merely a leaked handle.

## Acceptance

- [ ] `fargate` joins the closed backend vocabulary in its **attach** form: a task identity (ARN or
      service plus cluster) and a region, credential by Exchange reference, admitted through the
      standard handshake with remotely-reported provenance.
- [ ] A task that is stopped, pending, or running without the serving daemon fails closed with the
      faces distinguished; an `awsvpc` task's address obeys the binding's private-network scope
      (C-694).
- [ ] The change contains no task-creation, task-stop or scaling call, and says so in the reference
      docs alongside how an operator makes a task exist.
- [ ] The ephemeral half is recorded as gated: the story names the decision it needs, and the docs
      state plainly that a Fargate binding today attaches and never launches, so no reader infers
      per-wave tasks are available.
- [ ] A follow-up story for the ephemeral form is filed once that decision is accepted, including
      the teardown obligation and what happens to a task whose coordinator dies mid-selection.
