---
id: C-708
title: "A host declares a two-part locus and egress composite binding"
pillar: "Core"
status: backlog
epic: first-class-hosts
areas: [flux-system, flux-cli]
design: docs/designs/the-substrate-seam.md
note: "Decision 0020 rules 5-10: composite declared in configuration, indivisible locus, selectable egress with resolution following it, two-part SubstrateIdentity, whole-composite grant deny-by-default, confinement claim is the weaker part"
---

# A host declares a two-part locus and egress composite binding

## Goal

Let a host declare a composition of exactly two parts — one execution locus and one egress — as a
named binding in its own right, per accepted roadmap Decision 0020 (rules 5–10). The locus is
indivisible: process, workspace files, host files and env resolve to one backend. Egress is
selectable: dial, HTTP and name resolution resolve to one backend that may differ from the locus,
and the machine that dials is the machine that resolved. Descriptive families follow the locus and
are never forged. A composite's authority is granted as a whole — holding grants for both parts does
not confer the right to compose them — and its confinement claim is the weaker of its parts.

## Acceptance

- [ ] A composite host binding is declared in configuration like any other binding and names
      exactly one locus part and one egress part; a declaration that splits the locus families
      across backends is a configuration error, refused at load.
- [ ] Dial, HTTP and name resolution route to the egress backend while process, workspace files,
      host files and env route to the locus backend; the guard's private/loopback/link-local
      judgment runs on the coordinator over the answer the egress backend returned.
- [ ] `SubstrateIdentity` names both parts as a mapping rather than summarising them into one kind;
      non-composite bindings keep their existing identity shape and C-677's pass-through behaviour
      unchanged.
- [ ] A composite requires its own grant, deny-by-default: with grants held for both member
      bindings but none for the composite, selecting the composite refuses; widening to grant it is
      an escalation under the existing rules.
- [ ] A confined locus composed with an unconfined egress does not report itself confined; a
      posture floor that requires confinement evaluates the composite and refuses closed where the
      composition cannot satisfy it.
- [ ] Metrics describe the locus, and the audit record for an effect through a composite names the
      substrate per effect per Decision 0020 rule 4.
