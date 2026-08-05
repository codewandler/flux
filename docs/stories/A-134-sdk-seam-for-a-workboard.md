---
id: A-134
title: "One BoardRegistry and explicit BoardRef across SDK, model tools and fleet"
pillar: Agent
status: done
epic: first-class-board
design: docs/designs/native-board-fleet-cli.md
areas: [flux-datasource, flux-capabilities, flux-sdk, flux-orchestrate]
note: "Decision 0010 core — scope/profile/backend are independent; omitted board selectors refuse ambiguity"
---

# One BoardRegistry and explicit BoardRef across SDK, model tools and fleet

## Goal

Replace the implicit single-board assumption with one typed registry and explicit references while
preserving the delivered execution-board contract.

## Acceptance

- [x] Public contract types define stable `BoardId`, `BoardRef`, `BoardScope`, `BoardProfile`,
      `BoardBackend`, common item fields and planning-document references without adding IO at L0.
- [x] A registry binds several source-labelled boards, resolves operations by id/profile and rejects
      duplicate ids, invalid ids and scope/backend mismatches before a turn starts.
- [x] General, planning and execution profiles pin their exact state machines. All expose the common
      eight operations; planning adds metadata update; execution adds claim, dispatch recording and
      reassignment, retaining its existing eleven-operation surface.
- [x] One shared contract harness runs unmodified for every backend implementing a given profile.
      A backend cannot silently omit an operation or accept another profile's transition.
- [x] Planning document contracts cover vision/roadmap singletons and stable decision/design
      collections separately from queue items.
- [x] `ClientBuilder::try_with_board` installs board ops, group, ambient signal and permission
      resolver as one unit. Multiple registrations and built-in collisions return source-labelled
      build errors; no partial surface remains.
- [x] Fleet ledger calls carry a concrete `BoardRef`. Legacy omitted selectors work only with one
      compatible board and otherwise fail with every candidate listed.
- [x] Permission subjects are `board:<binding>/item/<id>` and federated writes retain the resolved
      member subject. Wildcards remain segment-scoped.
- [x] Failing-first SDK tests cover two boards with the same item id, ambiguous dispatch, profile
      transition refusal and atomic registration. Targeted tests pass; the board wave owns the gate.

## Notes

- This supersedes the earlier SDK-only scope of A-134; the SDK seam remains acceptance rather than
  becoming a separate competing registry.
