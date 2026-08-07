---
id: C-697
title: "A host binding resolves its credential through Exchange"
pillar: "Core"
status: backlog
epic: first-class-hosts
areas: [flux-secret, flux-cli]
design: first-class-hosts
note: "flux-secret's Scheme is Env | Plugin | Kubernetes; a binding whose credential lives in Exchange cannot be expressed, so every cloud host today needs a local env var"
---

# A host binding resolves its credential through Exchange

## Goal

Decision 0018 says a host names a credential *location*, never a value, and the existing vocabulary
covers `env/`, `plugin/` and `kubernetes/`. None of them fits a deployment whose vendor credentials
live where Decision 0007 put them: in Exchange, owner-authenticated, reachable only as a granted
operation. So a cloud substrate today requires an AWS or GCP credential in the operator's own
environment — precisely the channel Decision 0007 closed for connectors, reopened for hosts.

Add Exchange as a credential-reference scheme for host bindings. The binding names a grant, not a
secret; Exchange resolves it at connect time under its own admission rules; Flux never holds the
value, and the reference is what appears in config, logs, events and `flux host show`. This is the
enabler under every cloud host binding — the AWS ones (C-698, C-699) are its first consumers, and
it composes with the hosted single-org posture Decision 0019 established.

## Acceptance

- [ ] `flux-secret`'s reference vocabulary gains an Exchange scheme whose parts address a grant
      rather than a slot; a malformed or unknown reference fails closed at binding load, naming the
      binding.
- [ ] Resolution happens at connect time through the delivered Exchange client, under the grant's
      declared operation metadata; a caller — human or model — can never name the credential, the
      grant or the tenant, per Decision 0019's invariants.
- [ ] The value never enters Flux configuration, argv, logs, events, session state or
      `flux host show`/`probe` output; a test asserts the redaction on every one of those surfaces.
- [ ] An Exchange that refuses, is unreachable, or grants nothing yields a typed refusal naming
      which of those it was; nothing falls back to an environment variable or to an unauthenticated
      connection.
- [ ] The multi-tenant refusal stands: an Exchange in multi-tenant mode does not serve host
      credentials for a local-execution runtime, matching the existing invariant rather than
      carving an exception.
