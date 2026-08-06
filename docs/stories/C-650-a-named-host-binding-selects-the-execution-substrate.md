---
id: C-650
title: "A named host binding selects the execution substrate"
pillar: "Core"
status: backlog
epic: first-class-hosts
areas: [flux-cli, flux-runtime]
design: first-class-hosts
note: "Decision 0018 rules 2 and 4: --host NAME resolves a registered binding to the selected ExecutionSystem; --remote stays as ephemeral sugar"
---

# A named host binding selects the execution substrate

## Goal

A named binding selects the substrate. `--host <name>` resolves through the registry to the
`Arc<dyn ExecutionSystem>` installed into the execution environment — the path
`resolve_selected_execution_system` in `crates/flux-cli/src/execution.rs` walks today for the
anonymous `--remote <url>`, which remains as sugar constructing an ephemeral unnamed binding. The
Exchange catalogue binding likewise becomes nameable, giving the transitional
`FLUX_EXCHANGE_URL`/token env pair a declared home (the login journey itself is C-656).

## Acceptance

- [ ] `--host <name>` resolves a declared binding; an unknown name is a typed startup refusal that
      names the known bindings.
- [ ] `--remote <url>` behaves exactly as before and is recorded as an ephemeral binding.
- [ ] Substrate provenance on dispatch records carries the binding name alongside the existing
      kind and `remotely_reported` fields.
- [ ] Selection honors Decision 0018 rule 4: a binding not granted to the invoking surface refuses
      by default, and unattended/serving surfaces cannot widen a grant silently.
- [ ] A named exchange binding resolves origin and token reference where the transitional env pair
      is absent; the pair keeps working until C-656 retires it.
