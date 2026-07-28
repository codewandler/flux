---
id: C-90
title: Constrain plugin process capabilities by argument, not just program
pillar: Core
status: done
priority: 1
design: docs/designs/integration-plugins.md
note: the `process.run` gate matches argv[0] only, so granting `kubectl` grants delete/apply/exec — a read-only op's `Risk::Read` label is advisory, not enforced
---

# Constrain plugin process capabilities by argument, not just program

## Goal
Make the process capability as legible to the safety envelope as the HTTP one. Today a plugin's
`process` allow-list names programs; arguments are entirely plugin-controlled, so a grant of
`kubectl` is a grant of everything the ambient kubeconfig permits. An op declared read-only should
be structurally unable to mutate.

## Acceptance
- [ ] `PluginCapabilities` can constrain a granted program's arguments (design decision: per-program
      allowed leading subcommands, and/or a per-operation narrowing). Absent constraints keep
      today's behavior so existing manifests still load — the wire field is additive and optional.
- [ ] The host gate in `flux-plugin::host` rejects an undeclared argument shape before spawning,
      with the same deny-by-default posture as the `argv[0]` check. Failing-first test: a manifest
      granting `kubectl` + `get` denies `kubectl delete …`.
- [ ] `kubernetes` declares its constraints: read ops limited to `get`/`describe`/`logs`/`top`,
      mutation ops naming `scale`/`rollout`/`exec`/`port-forward` explicitly.
- [ ] `aws` follows with the same pattern, or records why it cannot.
- [ ] The authority requirement reflects the narrowing (a `process.exec` resource of `kubectl get`
      rather than bare `kubectl`) so approval prompts and audit records show what was actually
      allowed.

## Notes
- Context: the `process.run` gate is `crates/flux-plugin/src/host.rs` (allow-list matched against
  `argv[0]`, documented at `crates/flux-plugin/src/protocol.rs`). `process.spawn` uses the same gate.
- This is the reason the kubectl-subprocess model is weaker than the HTTP model, not the transport
  itself — see the decision record in `docs/designs/integration-plugins.md`. Closing this narrows
  the gap without a client rewrite, and it applies to every future CLI-driven plugin.
- Interacts with [C-89](C-89-process-authority-carrier.md), which made the process-mediated
  declaration expressible in the first place.
- Open design questions for the story's design pass: exact match vs glob on arguments; whether the
  constraint lives per-capability, per-operation, or both; how flags that change semantics
  (`kubectl get -o jsonpath` is harmless, `kubectl patch` is not) are handled without an allow-list
  that ossifies.
