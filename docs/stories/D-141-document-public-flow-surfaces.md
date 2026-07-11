---
id: D-141
title: Document the public flow surfaces
pillar: Agent
status: done
note: Map Flux-Lang, FlowEngine, Client, FlowClient, the Rust DSL, and advanced flow hosts in the public docs.
---

# Document the public flow surfaces

## Goal
Make the public documentation explain how flow-driven behavior reaches users through the language,
engine, SDK, CLI, programs, replay, and voice, with one clear recommendation for each embedding need.

## Acceptance
- [x] The public SDK guide distinguishes `Client`, `FlowClient`, the Rust DSL, `flux-lang`, and
  `flux-flow`, and no longer describes `Client` as a separate classic loop.
- [x] The `FlowClient` guide covers its builder, extension, lifecycle, result, optimization, voice,
  and suspension boundaries.
- [x] A website contract test pins the surface map and major `FlowClient` method families.
- [x] The crate README/rustdoc and public changelogs stay in sync with the website.

## Progress
- Audited `flux-lang`, `flux-flow`, `flux-agent`, `flux-app`, `flux-sdk`, the CLI, and the existing
  public language/agent documentation.
- Documented the recommended and advanced embedding paths and added drift protection.

## Notes
- Documentation-only; no runtime behavior or public Rust API changed.
