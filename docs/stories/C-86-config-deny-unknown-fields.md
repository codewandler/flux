---
id: C-86
title: Fail closed on typo'd security/budget config keys (deny_unknown_fields)
pillar: Core
status: done
priority: 13
epic: harness-hardening
design: docs/designs/harness-hardening.md
note: "Correctness (Medium) — a typo'd [server] introspection / [limits] budget key is silently dropped → fails open"
---

# Fail closed on typo'd security/budget config keys

## Goal
Make config parsing reject unknown keys everywhere it matters. Top-level `Config`, `SandboxConfig`, and
the adaptive/stage tables already `deny_unknown_fields` (documented as deliberate fail-safe), but
`[server]`, `[limits]`, `[workspace]`, `[skills]`, `[endpoint]`, `[private_net]`, and `[permissions]` do
not — so `[server] introspction_require_account = true` is silently dropped (auth check stays *off*) and
`[limits] turn_tokn_budget = 50000` runs *unbounded*. Both typos fail open on security/spend controls.

## Acceptance
- [ ] Failing-first test: an unknown key under `[server]`/`[limits]` is a parse error, not a silent drop.
- [ ] `#[serde(deny_unknown_fields)]` added to `ServerConfig`, `Limits`, `WorkspaceConfig`, and the other
      listed tables (verified none use `#[serde(flatten)]`, so it's safe).

## Progress
- (not started) — filed from the 2026-07-15 full code review.

## Notes
- `crates/flux-config/src/lib.rs:392` (`ServerConfig`), `:439` (`Limits`), `:278` (`WorkspaceConfig`),
  `:67/:94/:141`; existing deny sites at `:331`, `:303`, `:195/:216/:244` (rationale at `:299`, `:1690`).
- Design: [harness-hardening](../designs/harness-hardening.md).
