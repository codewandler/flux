---
id: C-554
title: "Remote agents join a fleet only through authenticated admission and leases"
pillar: Core
status: backlog
epic: remote-fleet-membership
design: docs/designs/remote-fleet-membership.md
areas: [flux-orchestrate, flux-a2a, flux-policy]
note: "follow-up remote boundary — invite, authenticated hello/capabilities, coordinator admission, expiry"
---

# Remote agents join a fleet only through authenticated admission and leases

## Goal

Make remote membership an explicit main-coordinator decision rather than a side effect of endpoint
discovery.

## Acceptance

- [ ] Main creates a bounded one-use invitation naming expected trust, capability ceiling and expiry.
- [ ] An authenticated hello binds stable remote identity, endpoint, capabilities, modes and fence
      posture; replay, identity substitution and capability widening fail closed.
- [ ] Admission/refusal is durable and source-labelled. Only admitted unexpired workers appear in
      fleet membership; configured or discovered endpoints remain candidates.
- [ ] Renewal preserves identity and can only narrow without a new admission. Expiry/cancellation
      stops new dispatch while preserving evidence for in-flight work.
- [ ] Permission subjects and audit events distinguish invitation, admission, lease and task egress.
- [ ] Protocol and adversarial tests cover replay, stolen invitations, stale leases and duplicate ids.
