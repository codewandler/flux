---
id: C-223
title: The pane.* ops — open, update, close, list, surfaced by sink presence at assembly time
pillar: Core
status: ready
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
- [ ] Effects are declared **accurately and minimally**: a pane touches no filesystem, no process and
      no network, so the effect set reflects that rather than over-declaring. `permission_subjects`
      returns the pane id, so a policy can scope panes by name — and per AGENTS.md an empty subject
      list is not an option for dodging the gate.
- [ ] **Surfaced by sink presence at assembly time**, not by a `ToolGroup` and not per-call. A session
      whose host installed no `SurfaceSink` never sees the ops in its catalog. **Failing-first test:**
      the assembled catalog contains the pane ops with a sink installed and omits them without one,
      and the decision is taken once — a mid-session change does not churn the tool set (A-95).
- [ ] A `pane.*` call reaching a context with no sink fails with a clear, actionable error — never a
      silent success. Test covers the headless path directly.
- [ ] `pane.update` on an unknown id, `pane.close` on an already-closed id, and a duplicate
      `pane.open` id each have defined, tested behaviour rather than a panic or a silent no-op.
- [ ] The op catalog is mirrored in `crates/flux-flow/docs/ops-reference.md`, and
      `crates/flux-cli/tests/website_contract.rs` is updated — an undocumented op fails the gate
      (C-208).
- [ ] `flux-cli`'s `run_tui` (`crates/flux-cli/src/app_cmd.rs`, ~`:690`) installs the TUI's
      `SurfaceSink`, so the ops are live on the daily driver and nowhere else.

## Progress
- (not started — depends on C-220, C-221, C-222)

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
