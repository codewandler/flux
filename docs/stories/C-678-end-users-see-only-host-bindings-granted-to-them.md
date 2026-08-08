---
id: C-678
title: "End users see only host bindings granted to them"
pillar: "Core"
status: backlog
epic: first-class-hosts
areas: [flux-cli]
design: first-class-hosts
note: "Decision 0018 rule 4's second sentence is unimplemented across the flux host surface; C-654's review measured the gap"
---

# End users see only host bindings granted to them

## Goal

Decision 0018 rule 4 says host authority is granted, never ambient — and its second sentence goes
further: infra-layer bindings are not end-user-selectable, and "end users see only bindings
granted to them." Selection honors this (`resolve_named_host` is grant-checked, deny-by-default),
but visibility does not: `session_host_registry` loads the store, merges config, and returns
everything, so `flux host ls`/`show`/`probe`/`metrics` — and the agent-visible `host.*`
operations — enumerate every declared binding regardless of grant. C-654's review measured this
as pre-existing across the whole surface and routed it here. Filter the read surface by the same
grant vocabulary selection already uses, so a binding granted to nobody (the default) or to a
different layer never renders for a principal it was withheld from.

## Acceptance

- [ ] `flux host ls`/`show` and the `host.list`/`host.info` operations render only bindings whose
      grant admits the calling surface; an ungranted binding is absent, not redacted.
- [ ] `probe` and `metrics` refuse an ungranted binding with the grant-refusal face selection
      already uses, instead of resolving its credential.
- [ ] The operator seat retains a deliberate everything view (`flux host ls --all` or equivalent)
      that says it is showing withheld bindings.
- [ ] A test proves an infra-layer binding invisible to the interactive surface and visible to
      the layer it names; the story documents that the ambient `host` group gate is unchanged.
