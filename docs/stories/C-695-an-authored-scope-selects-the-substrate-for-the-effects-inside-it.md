---
id: C-695
title: "An authored scope selects the substrate for the effects inside it"
pillar: "Core"
status: backlog
epic: first-class-hosts
areas: [flux-lang, flux-runtime]
design: docs/designs/the-substrate-seam.md
note: "--host is session-immutable by design and a target is unavailable to model-authored input; per-call placement must therefore be an authored scope, never a tool parameter"
---

# An authored scope selects the substrate for the effects inside it

## Goal

Today a substrate is chosen once: `--host <name>` is immutable for the session and inherited by
sub-agents, and a remote target is deliberately unavailable to model-authored input. That
invariant is load-bearing — if a substrate were a tool parameter, the model would be choosing
where effects land, and every authorization decision made about a binding could be sidestepped by
a generated argument.

But one session frequently needs two substrates: read a file locally, call a service inside a
cluster; run the build on the build host, publish from here. The way to have both without
surrendering the invariant is to make placement an **authored scope** rather than a parameter —
the operator writes it in Flux-Lang, the model runs inside whatever scope it was given and cannot
name its own. Placement stays operator-chosen; only its granularity changes from a session to a
lexical region.

This story is gated on a decision amending "immutable for the session" to "immutable within its
authored scope", including what a sub-agent spawned inside a scope inherits, and what an
in-flight effect sees when a scope exits.

## Acceptance

- [ ] An authored construct binds a declared host for the effects lexically inside it; the binding
      must be granted to the surface exactly as `--host` requires, and the grant is checked when
      the scope is entered, not when the effect runs.
- [ ] No model-authored input can enter, leave, or name a scope: a generated tool argument cannot
      select or change a substrate, and a test proves the refusal.
- [ ] A sub-agent spawned inside a scope inherits that scope's substrate and cannot widen it; the
      selection a child carries is still the parent's selection, never a resolved system.
- [ ] Scope exit is deterministic for in-flight work: an effect that began inside a scope completes
      against the substrate it started on, and the audit record names the substrate per effect
      rather than per session.
- [ ] With no authored scope, `--host` behaves exactly as it does today, session-immutable.
