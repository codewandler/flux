---
id: C-219
title: "The agent-authored surface — panes the model opens, config it can safely change (epic)"
pillar: Core
status: ready
priority: 10
epic: agent-authored-surface
design: docs/designs/agent-authored-surface.md
areas: [flux-tui]
note: "the tool→surface seam already exists twice (ToolProgressSink/SpawnActivitySink) and op.register already lets the model extend the harness one layer down — what's missing is the surface layer, and the whole risk is that a model-drawn region can imitate the approval sheet"
---

# The agent-authored surface — panes the model opens, config it can safely change (epic)

## Goal
Let the main agent shape the harness it runs in: open, update and close **typed panes** on the TUI —
containers for results, live info and sub-agent fleets — and change an **allowlisted** set of flux
config keys, applying them to the session it is already in.

The surface keeps geometry, theming and trust markers. The model supplies data. That split is the
whole design, and it is what makes "completely free" safe to grant.

## Acceptance
- [ ] C-220 (the L2 `SurfaceSink` contract), C-221 (pane slots in the TUI), C-222 (the trusted-chrome
      invariant), C-223 (the `pane.*` ops), C-224 (the sub-agent fleet pane) and C-225 (agent-writable
      config keys) are done, each with the failing-first test its story names.
- [ ] A model-issued `pane.open` produces a live pane on `flux tui`, updatable across turns, and
      `pane.*` fails clearly — never silently — on a surface that installed no sink.
- [ ] **The two structural safety properties hold and are tested, not asserted:**
      a pane whose payload is verbatim approval-sheet text still renders inside the marked
      agent-owned region with the sheet drawn over it (C-222); and the agent-writable config key set
      is **disjoint from `flux_config::PinnableKey::ALL`** by unit test, so `[permissions]`,
      `[sandbox]`, `workspace.allow_all` and `private_net.web` are unreachable rather than denied
      (C-225).
- [ ] The sub-agent fleet pane renders A-79's existing `SpawnActivity` stream, which the TUI
      currently discards — visible value independent of the model ever calling `pane.open`.
- [ ] `cargo test -p flux-codegate` stays green: no L2→L6 edge is introduced. The surface contract
      lives at L2 and the surface installs it, exactly as `ToolProgressSink` does.
- [ ] Full gate green; the `register_builtins` expected-names test and
      `crates/flux-cli/tests/website_contract.rs` updated for the new ops.

## Progress
- 2026-07-29 — epic opened from a direct request ("tools to directly modify the harness, the UI …
  widget tools … or simply config … so it would be completely free"). Design:
  [agent-authored-surface.md](../designs/agent-authored-surface.md). Every claim in the design was
  verified against the tree before filing: the two existing sink contracts, `op.register`'s scope
  ladder, the fixed six-row layout, the `PinnableKey` list, the existing atomic config persisters,
  and the fact that `flux-tui` installs no `SpawnActivitySink`.
- Two settled decisions, both narrowing scope deliberately: a **typed pane vocabulary** rather than
  raw layout control, and an **allowlisted hot-key** config surface rather than any-key-plus-approval
  or process re-exec. Rationale for each is in the design's *Alternatives considered*.
- Ordering: C-220 → C-221 → C-222 **before** C-223. The contract, the rendering and the trust
  invariant all land before the model can reach any of it. C-224 and C-225 are separable.

## Notes
- **Why the trust invariant is its own story and not a checkbox on the renderer.** The failure mode
  is not a bug in pane rendering; it is a pane rendering *correctly* and being mistaken for harness
  chrome. That needs its own adversarial test — a payload chosen to impersonate — and its own review.
  C-163 reached the same conclusion for plugins and wrote it into its acceptance rather than its
  notes.
- **Why the config half is not "just a tool".** `config.set` is the first model-facing op that
  writes a file whose contents feed the safety envelope on the next read. The allowlist being
  *disjoint from `PinnableKey`* — machine-checked, not reviewed — is what keeps it from being a
  bypass. See [C-225](C-225-agent-writable-config.md).
- **No process re-exec, deliberately.** A self-restart is a new turn-termination path, the bug class
  AGENTS.md flags as having recurred three times. `/resume`'s existing `project_session` gives the
  same outcome inside the process.
- **Not a `ToolGroup`.** Groups are signal-gated (`flux-tools/src/groups.rs:9-28`) and there is no
  `project.signal` for "a human is watching a terminal". Sink presence at assembly time surfaces the
  ops — the `[consult] model` precedent and the A-95 cache-stability lesson.
- Adjacent, and deliberately **not** in this epic: [A-47](A-47-tui-time-machine-cockpit.md) (the
  replay cockpit becomes a pane once slots exist) and [C-163](C-163-plugin-commands-and-host-ui.md)
  (plugin host UI must reuse `SurfaceSink` rather than open a second UI path — say so when C-163 is
  designed).
- ⚠ `ratatui` is held at 0.29 by `markdown-ratatui` (root `Cargo.toml:131-136`), the same hold behind
  [C-205](C-205-bump-lru-drop-unsound-ignore.md)'s neighbourhood. This epic adds **no** widget
  dependency and builds only on primitives the TUI already uses.
