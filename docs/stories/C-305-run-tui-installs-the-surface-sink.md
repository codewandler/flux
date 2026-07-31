---
id: C-305
title: "`run_tui` installs the TUI's SurfaceSink, so the pane.* vocabulary stops being inert"
pillar: Core
status: ready
priority: 8
epic: agent-authored-surface
design: docs/designs/agent-authored-surface.md
areas: [flux-cli, flux-tui]
note: "C-223 shipped pane.open/update/close and the fail-closed surfacing seam, but nothing calls try_register_surface_ops — run_tui builds the agent BEFORE the surface exists, so the sink must be minted ahead of build_agent for the decision to stay an assembly-time one"
---

# `run_tui` installs the TUI's `SurfaceSink`, so the `pane.*` vocabulary stops being inert

## Goal

Make the ops C-223 landed actually reachable: mint the TUI's `SurfaceSink` before the agent is
assembled, and pass its presence into `flux_tools::try_register_surface_ops` so a TUI session
advertises `pane.open` / `pane.update` / `pane.close` and a headless run still advertises nothing.

## Why this is its own story

C-223 delivered the ops and the surfacing mechanism, and deliberately stopped at the fence it was
given. The wiring is **not** a one-liner and that is the whole point: `run_tui` builds the agent
*before* `flux_tui::run_with_options` creates the surface, so the sink does not exist at the moment
the catalog is assembled. Satisfying C-223's "decided once, at assembly time" property means minting
the sink ahead of `build_agent`, which is a build-order change rather than an extra argument.

Until this lands the vocabulary is dead code: nothing calls `try_register_surface_ops`, so no model
ever sees a `pane.*` op. **The first symptom of getting this wrong is silence — "the model never
opens a pane" — not a crash.**

## Acceptance

- [ ] `run_tui` mints the TUI's `SurfaceSink` **before** `build_agent`, and passes
      `surface_sink_installed = true` into `flux_tools::try_register_surface_ops`.
- [ ] **Failing-first test**: a TUI-shaped assembly advertises `pane.open`; it fails today because
      nothing registers the ops.
- [ ] **A headless assembly still advertises nothing** — `flux run`, `flux-server` and an SDK
      embedding must not gain `pane.*`. This is the fail-closed half and it is the one that matters:
      a registered op with no `group` is advertised **unconditionally**, which is exactly why C-223
      kept these out of `register_builtins`. Assert both directions.
- [ ] The decision is made **once** at assembly and does not churn if the sink later goes away —
      C-223 already has `the_surfacing_decision_does_not_churn_when_the_sink_goes_away`; keep it true
      through the real wiring.
- [ ] An end-to-end check that a `pane.open` call from a model actually reaches `flux-tui`'s pane
      state, rather than only that the op is advertised.
- [ ] Full gate green.

## Notes

- ⚠ **Do not "fix" this by moving the ops into `register_builtins`.** That surfaces them in every
  headless catalog and breaks C-223's Acceptance item 3. Conditional registration in the assembly
  path is the only seam that fails closed; the precedent C-223 followed is `consult`
  (`crates/flux-cli/src/execution.rs`).
- ⚠ `Conditional` idempotency on these ops is load-bearing. Flipping it to `Idempotent` puts them in
  the dispatcher's op cache, and a cached hit returns **without executing** — so the surface would
  silently never see a repeated update.
- Related: [C-223](C-223-pane-ops.md) shipped the ops; [C-306](C-306-pane-read-back-contract.md) owns
  the `pane.list` / read-back contract question, which this story does not need.
