---
id: C-223
title: The pane.* ops — open, update, close, list, surfaced by sink presence at assembly time
pillar: Core
status: in-progress
priority: 14
epic: agent-authored-surface
design: docs/designs/agent-authored-surface.md
areas: [flux-tools]
note: "NOT a ToolGroup — groups are signal-gated (groups.rs:9-28) and there is no project.signal for 'a human is watching a terminal'; the precedent is [consult] model, whose mere presence surfaces the op once at assembly time and never churns (the A-95 cache-stability lesson)"
---

# The `pane.*` ops

## Goal
Give the model the vocabulary: `pane.open` / `pane.update` / `pane.close` / `pane.list` in
`flux-tools`, delegating through the audited dispatcher to the host's `SurfaceReporter`. This is the
story that makes the surface reachable — and it lands only after the contract (C-220), the rendering
(C-221) and the trust invariant (C-222) are already in.

## Acceptance
- [ ] Four ops in a new `crates/flux-tools/src/surface.rs`, registered in `register_builtins`, each
      implementing `flux_runtime::Tool` with spec + `permission_subjects` + `intents` + `execute`,
      IO via `ctx` only. The `register_builtins` expected-names test is updated (it fails otherwise).
      → **three** of four landed (`pane.open`/`update`/`close`), each with all four trait members and
      no IO but `ctx.surface()`. `pane.list` is **not implementable** on C-220's contract: the
      channel is send-only by design (`flux-runtime/src/lib.rs:461`), so there is no read-back, and
      the tool may not keep its own copy of the store. Registration is
      `flux_tools::try_register_surface_ops`, not `register_builtins` — see the item below for why
      the two cannot both hold; `builtins_register` is therefore unchanged.
- [x] Effects are declared **accurately and minimally**: a pane touches no filesystem, no process and
      no network, so the effect set reflects that rather than over-declaring. `permission_subjects`
      returns the pane id, so a policy can scope panes by name — and per AGENTS.md an empty subject
      list is not an option for dodging the gate.
      → `surface.rs` `pane_spec` (empty `effects`/`access`, `human_visible` semantic effect,
      `Conditional`), proven by `the_pane_pack_declares_coherent_metadata_and_authority` and
      `every_pane_op_names_the_pane_it_addresses`.
- [x] **Surfaced by sink presence at assembly time**, not by a `ToolGroup` and not per-call. A session
      whose host installed no `SurfaceSink` never sees the ops in its catalog. **Failing-first test:**
      the assembled catalog contains the pane ops with a sink installed and omits them without one,
      and the decision is taken once — a mid-session change does not churn the tool set (A-95).
      → `try_register_surface_ops(registry, surface_sink_installed)`, fail-closed;
      `the_pane_ops_are_surfaced_only_for_a_host_with_a_sink` +
      `the_surfacing_decision_does_not_churn_when_the_sink_goes_away`. No group, no signal.
- [x] A `pane.*` call reaching a context with no sink fails with a clear, actionable error — never a
      silent success. Test covers the headless path directly.
      → `every_pane_op_fails_actionably_without_a_sink`.
- [ ] `pane.update` on an unknown id, `pane.close` on an already-closed id, and a duplicate
      `pane.open` id each have defined, tested behaviour rather than a panic or a silent no-op.
      → duplicate `open` (replaces in place) and repeated `close` (no error, no change) are defined,
      documented in the op descriptions and tested
      (`reopen_unknown_update_and_repeated_close_are_all_defined`). An `update` for an id that is not
      open stays a **silent** host-side drop (`flux-tui/src/panes.rs:219-225`): with no read-back the
      op cannot detect it, so the model is only *warned* in the description. Closing that gap needs
      the same read-back `pane.list` needs.
- [x] The op catalog is mirrored in `crates/flux-flow/docs/ops-reference.md`, and
      `crates/flux-cli/tests/website_contract.rs` is updated — an undocumented op fails the gate
      (C-208).
      → both references carry a table row; verified by *removing* the rows and watching
      `operations_reference_covers_the_registered_public_catalog` and
      `the_in_repo_reference_covers_the_whole_production_catalog` fail. `catalog_coherence.rs`'s
      census registers the pack (decision on) so the risk column is checked too.
- [ ] `flux-cli`'s `run_tui` (`crates/flux-cli/src/app_cmd.rs`, ~`:690`) installs the TUI's
      `SurfaceSink`, so the ops are live on the daily driver and nowhere else.
      → not done: outside this story's dispatched fence (`flux-tools` + the two references), and it
      is not a one-liner — `run_tui` builds the agent *before* `flux_tui::run_with_options` creates
      the surface, so the sink has to be minted ahead of `build_agent` for the surfacing decision to
      be an assembly-time one. Until that lands the vocabulary is inert: nothing registers it.

## Progress
- Landed the send half of the vocabulary in `crates/flux-tools/src/surface.rs`: `pane.open`,
  `pane.update`, `pane.close`, each validating its shape and then delegating to the host through
  `ctx.surface()` (`op.register`'s posture — no pane state of its own).
- **Surfacing mechanism:** `try_register_surface_ops(registry, surface_sink_installed)` — a
  fail-closed registrar the assembling host calls once, mirroring how `[consult] model` registers
  `consult` in `execution.rs`. Deliberately not a `ToolGroup`.
- Not `register_builtins`, and this is the one real design decision to review: a registered op with
  no group is advertised unconditionally (`ToolRegistry::active_specs`/`is_advertised` gate on groups
  only), so registering the pane ops there would put them in *every* headless catalog — exactly what
  the "surfaced by sink presence" item forbids. Conditional registration in the assembly path is the
  only seam that fails closed, and it is the seam the story's own `consult` precedent uses.
- **Two items remain, and both need one thing C-220 did not build: a read-back.** `pane.list` (the
  design's own open question) and a non-silent `pane.update` for an unknown id both require the
  surface to answer a query. `flux-tui` already has the answer (`ChatState::open_panes`,
  `panes::PaneListing`) and its doc comment expects C-223 to wire it, but the wire needs a
  `SurfaceSink` query method plus an L2 listing type — a change to C-220's contract, which is a
  design decision rather than an implementation detail. Either extend the contract in a follow-up or
  drop `pane.list` from the vocabulary; do not let a `pane.list` ship that reports the model's own
  guesses.
- Next step for the epic: the `run_tui` wiring above (which is what makes any of this reachable),
  then the read-back decision.

## Notes
- Copy `op.register`'s posture (`crates/flux-tools/src/reflect.rs:459-503`): validate the shape, then
  delegate to the host, which owns all state mutation. The tool holds no pane state of its own.
- The op description is model-facing and load-bearing — say what a pane is *for* (a durable container
  for results/status the user should keep seeing) and what it is not (a place to put the answer; the
  transcript is still where prose goes). `op.register`'s description is a good model for tone.
- Keep results terse. These ops may be called every round; a chatty result inflates every subsequent
  prompt.
- **Do not add a `surface` `ToolGroup`.** It reads like the obvious mechanism and it is the wrong one
  — see the note field and the design's *Surfacing, not gating*.
