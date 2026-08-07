---
id: C-698
title: "An EC2 host binding reaches a running instance"
pillar: "Core"
status: backlog
epic: first-class-hosts
areas: [flux-cli, flux-system]
design: first-class-hosts
note: "an instance is long-lived and already exists, so it composes exactly like microvm: the remote protocol served on the guest, with the credential from Exchange"
---

# An EC2 host binding reaches a running instance

## Goal

An EC2 instance is the cloud case that fits the current rules without amending them: it is
long-lived, it exists before Flux is asked about it, and reaching it is the composition Decision
0018 rule 3 already names — the delivered remote protocol served on the guest from C-480's VM
profile, admitted by handshake. What EC2 adds over `microvm` is identity and discovery: an instance
is named by instance-id or tag rather than by a URL an operator pastes, and the credential that
finds it belongs in Exchange (C-697) rather than in the operator's environment.

The provisioning boundary holds exactly as it does for every other binding: Flux never launches,
stops, terminates or resizes an instance. A binding consumes an endpoint that already exists, and
the AWS plugin's `aws.ec2.instances` inventory is how an operator finds out what exists.

## Acceptance

- [ ] `ec2` joins the closed backend vocabulary, declarable with an instance identity (id or a tag
      selector) and a region, with its credential as an Exchange reference; an ambiguous tag
      selector is a loud refusal rather than an arbitrary pick.
- [ ] Resolution turns the instance identity into the served endpoint using read-only inventory
      under the granted credential, then admits it through the standard remote-protocol handshake;
      `flux host probe` reports the negotiated version and the guest's `SubstrateIdentity` with
      remotely-reported provenance.
- [ ] An instance that is stopped, absent, or running without the serving daemon fails closed with
      the three faces distinguished by name; nothing falls back to local execution and nothing
      starts anything.
- [ ] No provisioning surface exists anywhere in the change — no run/stop/terminate/modify call —
      and the reference docs point at C-480's VM profile as how the endpoint comes to exist.
- [ ] Reaching a private-address instance obeys the binding's own private-network scope (C-694)
      rather than a caller-wide allowance, and a private-CA certificate resolves through C-684.
