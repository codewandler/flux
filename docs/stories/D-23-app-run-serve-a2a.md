---
id: D-23
title: Serve agents through flux app run
pillar: Agent
status: done
priority:
design:
---

# Serve agents through flux app run

## Goal
Collapse the standalone `flux serve` daemon into `flux app run --serve` and make A2A a program channel, so
there is one daemon command for both declarative apps and ad-hoc served agents.

## Acceptance
- [x] `flux serve` is removed from the command tree; `flux app run --serve <addr> --yes` serves the built-in
      coding agent with the previous REST/SSE/A2A surface.
- [x] `.flux` programs can expose an agent through a declared `a2a` channel, and `flux app run
      <program.flux> --serve <addr>` injects an ad-hoc A2A channel for a sole-agent program.
- [x] The agent card is parameterized from the served program agent instead of hard-coded to the built-in
      coding agent.
- [x] Non-loopback binds still require `FLUX_SERVER_TOKEN`; the no-program served coding-agent path still
      requires `--yes`.
- [x] Touched crate tests pass: `cargo test -p flux-server -p flux-app -p flux-channels -p flux-cli`.

## Progress
- **Done (2026-06-30).** Added the `a2a` channel adapter in `flux-channels`, exposed `flux_server::router`
  for reuse, threaded card metadata through the server state, and replaced `flux serve` with
  `flux app run --serve`.

## Notes
- Follow-up verification before release: manual smoke the no-program path, declared channel path, and
  ambiguous multi-agent error after the full workspace gate is green.
