---
id: D-95
title: Direct plugin-call private-net grant parity + scoped-egress docs
pillar: Core
status: done
priority: 6
epic: gitlab-plugin-hardening
design: docs/designs/gitlab-plugin-hardening.md
note: "endpoint-level private-net grants parse in config but are ignored by direct `flux plugin call` (only [private_net.plugins] is wired there); document the scoped-egress recipe for testing a private endpoint safely (GL-002/003); extends D-20"
---

# Direct plugin-call private-net grant parity + scoped-egress docs

## Goal
Bring the direct `flux plugin call` path to parity with the design's private-net grant model, or
document its actual support honestly, and give operators a safe scoped-egress recipe for testing a
private endpoint. Extends the [D-20](D-20-scoped-private-net-egress.md) scoped-egress model.

## Why (evidence)
A beta pass found that the config layer parses endpoint-specific grants
(`[private_net.endpoints]`) and the design describes per-endpoint grants, but a direct
`flux plugin call` still refused a private endpoint under an endpoint-level grant — the direct path
passes only `cfg.plugin_private_hosts(&manifest.name)` into the host caps, so only
`[private_net.plugins]` is consulted. The default refusal itself is correct and verified; the gap is
that an accepted config shape is silently ineffective on that path.

## Acceptance
- [ ] Endpoint-level grants are wired into the direct `flux plugin call` path, **or** the docs +
      config surface state clearly that direct invocation supports `[private_net.plugins]` only, and
      an endpoint-only grant is reported (not silently ignored) (GL-003).
- [ ] A short "testing a private endpoint safely" note is added to the plugin/QA docs, showing the
      per-plugin scoped grant and warning against a global private-net grant — including that
      `[private_net.plugins] <p> = true` is broad for a plugin whose manifest declares
      `private_hosts = ["*"]` (GL-002).
- [ ] A test asserts an endpoint-only grant on the direct path either admits egress (if wired) or
      surfaces a clear "not consulted on this path" diagnostic (no silent refusal).
- [ ] `cargo build/test/clippy -D warnings/fmt` green.

## Progress
- Not started.

## Notes
- Ties into the D-20/D-30 endpoint-grant + audit work and the D-65 app-path redaction/audit parity.
- Docs live under the public plugin/QA docs; keep the recipe generic (no consumer/host specifics).
