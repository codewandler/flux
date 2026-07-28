---
id: C-165
title: Managed config tier — an enforced baseline a local user cannot override
pillar: Core
status: backlog
priority:
epic:
design:
note: "config is a two-layer user→project merge (flux-config lib.rs:968-973) where BOTH layers are writable by the same user, so there is no way to pin a policy floor an operator can't edit — the landscape doc names regulated/auditable buyers as flux's whitespace, and this is the missing half of that story; needs no backend"
---

# Managed config tier — an enforced baseline a local user cannot override

## Goal
Let an organization pin a floor. Today `load()` merges exactly two layers — the user's home config
and the project's `.flux/config.toml` (`crates/flux-config/src/lib.rs:968-973`) — and both are
writable by the person running flux, so every setting is advisory. A **managed** layer (a
system-owned path, or one pinned by an environment channel) that takes precedence over both, and
whose security-relevant keys cannot be relaxed downstream, turns flux's default-deny envelope from
"the default a developer accepted" into "the baseline an auditor set".

## Acceptance
- [ ] A third config layer loads ahead of user and project, from a documented system location
      (plus an explicit override channel for containerized deploys) — failing-first test asserting
      precedence over both existing layers.
- [ ] The layer distinguishes **defaults** (a starting value the user may change) from **pins** (a
      value the user may not change). A downstream layer attempting to relax a pinned
      security-relevant key is refused with a named diagnostic, not silently ignored — test covers
      both a permitted override and a refused one.
- [ ] Relaxation is refused in the *permissive* direction only: a project may still make itself
      **more** restrictive than the managed baseline. Pinned by test in both directions.
- [ ] The effective configuration is inspectable — one command shows each setting's value and which
      layer it came from, so "why can't I enable this" has an answer (natural home: the C-128
      `flux doctor` diagnostics if that lands first).
- [ ] The managed file's own trust is stated honestly in the docs: this is an **operator** control
      backed by filesystem permissions, not a defense against a user who owns the machine and can
      edit the binary. Overclaiming here would be worse than not shipping it.
- [ ] Website security docs updated truthfully (the C-16 / L-19 / D-137 docs-truth pattern).

## Progress
- (not started — filed from the 2026-07-28 Amp feature-mining pass, second pass)

## Notes
- Source: [../research/amp.md](../research/amp.md) — Amp's Enterprise "managed settings
  (system-wide enforcement)". **This story exists because the first mining pass got it wrong**: it
  was bulk-rejected under "enterprise features need a hosted control plane." Managed settings need
  no backend at all — they are a file and a precedence rule.
- Evidence the gap is real: `crates/flux-config/src/lib.rs:968-973` — `load()` is
  `merge(user, project)`, full stop.
- Strategic weight: [`../archive/research/landscape.md`](../archive/research/landscape.md) Part 2
  argues flux's open lane is local-first + auditable + default-deny for regulated buyers. A policy
  floor a developer cannot silently lower is the missing half of that claim — today an auditor has
  to trust that nobody edited `.flux/config.toml`.
- Interacts with the authorization policy and the sandbox config (D-134: `require` mode) — decide
  deliberately which keys are pinnable, and keep that list small and security-relevant rather than
  making everything pinnable because it is easy.
