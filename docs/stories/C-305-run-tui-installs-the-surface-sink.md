---
id: C-305
title: "`run_tui` installs the TUI's SurfaceSink, so the pane.* vocabulary stops being inert"
pillar: Core
status: done
epic: agent-authored-surface
design: docs/designs/agent-authored-surface.md
areas: [flux-cli, flux-tui, flux-flow]
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

- [x] `run_tui` mints the TUI's `SurfaceSink` **before** `build_agent`, and passes
      `surface_sink_installed = true` into `flux_tools::try_register_surface_ops`.
      → `app_cmd.rs` `run_tui` mints `flux_tui::PaneQueue` then calls
      `build_agent_with_surface(&flags, panes.clone())`; `execution.rs` `build_agent_with` registers
      with `surface_sink.is_some()`. Pinned by
      `catalog_coherence::the_pane_surfacing_decision_comes_from_the_assembling_surfaces_own_sink`
      (a source census, since `build_agent_with` cannot be called from a test) and by
      `app_cmd::tui_surface_wiring::the_options_carry_the_very_pane_channel_the_agent_was_given`.
- [x] **Failing-first test**: a TUI-shaped assembly advertises `pane.open`; it fails today because
      nothing registers the ops.
      → `crates/flux-cli/tests/pane_surface_wiring.rs::a_model_pane_open_reaches_the_tui_pane_state`.
- [x] **A headless assembly still advertises nothing** — `flux run`, `flux-server` and an SDK
      embedding must not gain `pane.*`. This is the fail-closed half and it is the one that matters:
      a registered op with no `group` is advertised **unconditionally**, which is exactly why C-223
      kept these out of `register_builtins`. Assert both directions.
      → `a_headless_assembly_never_advertises_or_dispatches_a_pane_op` (absent from the registry AND
      refused at dispatch); `build_agent`/`build_agent_lazy` pass `None`, and
      `build_agent_with_surface` is the only path that passes a sink.
- [x] The decision is made **once** at assembly and does not churn if the sink later goes away —
      C-223 already has `the_surfacing_decision_does_not_churn_when_the_sink_goes_away`; keep it true
      through the real wiring.
      → still green, plus
      `the_advertised_catalog_does_not_churn_when_the_surface_handle_goes_away` over a real engine.
- [x] An end-to-end check that a `pane.open` call from a model actually reaches `flux-tui`'s pane
      state, rather than only that the op is advertised.
      → `a_model_pane_open_reaches_the_tui_pane_state`: a scripted model calls `pane.open` inside a
      real turn; the surface side then goes through `flux_tui::session_state` and the production
      event loop (`drive_event_loop_headless`), and the pane is read back out of
      `ChatState::open_panes` **and** off the frame that was drawn. **Not** an assertion that a sink
      object was installed — see Progress for what that distinction cost, and for the round-two
      finding that the last two links were reconstructed rather than invoked.
- [x] Full gate green.

## Progress

- **The wiring was three links, not one, and the middle one is the story's real content.**
  `run_tui` minting the sink and `build_agent_with` registering the ops is the half the story
  describes. The half it does not: **`flux-flow` had no way to carry a sink into a turn at all.**
  Every turn runs inside `scope_runtime_turn(RuntimeTurnContext…)`, and
  `ToolContext::runtime_turn_context` documents that an active lexical scope is authoritative
  *including its absent fields* — so a sink installed the obvious way, on the executor's stored
  fallback via `ToolContext::set_surface_sink`, is invisible to every op in every real turn.
  `ctx.surface()` would return `None`, `pane.open` would fail with "no surface", and the wiring
  would read as complete. `FlowEngine` therefore grew a `surface_sink` field
  (`engine.rs`), re-attached to each turn's context in `begin_turn`. This is why the end-to-end test
  runs an actual turn: a test that dispatched `pane.open` directly would have passed against the
  broken wiring.
- **Delivery, not installation.** Each link was verified by mutation rather than by reading:
  - dropping the `begin_turn` re-attach → `a_model_pane_open_reaches_the_tui_pane_state` reds;
  - `try_register_surface_ops(&mut registry, true)` (or `false`) hard-coded →
    `the_pane_surfacing_decision_comes_from_the_assembling_surfaces_own_sink` reds. Worth noting
    that before that test existed, **hard-coding `false` left every other test in the repo green** —
    the C-299/C-307 failure mode, reproduced;
  - dropping `options.pane_queue` → `the_options_carry_the_very_pane_channel_the_agent_was_given`
    reds.
- **Shape:** `flux_tui::PaneQueue` is the sink — a bounded queue rather than a direct call, because
  the agent is assembled before `ChatState` exists. `ChatState::apply_pending_panes` drains it at the
  top of the event loop, *before* the `UiEvent` drain, so a late command cannot resurrect a
  `turn`-lifetime pane that `Finished` just expired. Mirrors A-94's `SteeringQueue` in the other
  direction.
- **Public catalog is unchanged** — no new ops, so both reference mirrors stay as C-223 left them.
  `flux_spec::metadata_violations` re-verified over the assembled TUI catalog, and
  `permission_subjects` still returns the pane id (`the_tui_pane_catalog_declares_honest_metadata_and_names_its_subjects`).
- **Round two: the last two links were reconstructed, not invoked — now they are pinned.** Review
  found that deleting *both* `state.apply_pending_panes()` (in `event_loop`) and the
  `options.pane_queue` → `with_pane_queue` install (in `run_with_options`) left the whole flux-tui +
  flux-cli surface green, because the end-to-end test rebuilt those two links by hand. The fix is a
  seam rather than a stronger assertion:
  - `run_with_options` no longer builds the `ChatState` inline. `session_state(&agent, &session_id,
    &options)` does, and it holds the install. It is callable without a TTY.
  - `event_loop` is generic over `B: Backend` and takes its event stream as a parameter. It touched
    the concrete terminal in exactly one place (`terminal.draw`) and crossterm in exactly one
    (`input.next()`), so this is a two-signature change with one production call site.
    `drive_event_loop_headless` runs it over a `TestBackend` and an **empty** event script — the
    stream's immediate `None` is the loop's own end-of-input exit, so it draws one frame and stops.
  Both are `#[doc(hidden)] pub` for one reason: `flux-cli` is the only crate that can assemble a
  `FlowEngine` to hand them, because `flux-tui` does not depend on `flux-system` and adding that
  dependency was out of scope.
- **Each of the two is now pinned by a test that reds for it alone** (`pane_surface_wiring.rs`):
  `the_state_the_tui_assembles_carries_the_pane_channel_the_agent_writes_to` reds only on the
  install, `the_event_loop_drains_the_agents_pane_channel_into_the_frame_it_draws` reds only on the
  drain, and `a_model_pane_open_reaches_the_tui_pane_state` — now running the whole chain through
  the production path — reds on either. Verified by applying each deletion to the shipped line and
  reverting.
- **The census guard now pins the *sense* of the decision, not only its receiver.**
  `registration_source` rendered `surface_sink.is_some()` as `"surface_sink"`, so inverting it to
  `.is_none()` — the mutation that fails *open*, advertising `pane.*` in every headless `flux run`,
  `flux-server` and SDK catalog — left
  `the_pane_surfacing_decision_comes_from_the_assembling_surfaces_own_sink` green. Non-`clone`
  method calls now render as `receiver.method()`.
- **Still not covered, stated rather than implied:** the queue's overflow path
  (`panes.rs`'s `MAX_PENDING_COMMANDS`) drops the newest command and returns ok, with no evidence
  that it happened — against `surface.rs`'s posture that the sibling failure is "a clear op failure
  (never a silent success)". Drop-newest is the right choice of the two; the missing piece is the
  observation, and it is a UX decision (a transcript notice? an op failure?) rather than a
  mechanical gap, so it is left for its own story.

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
