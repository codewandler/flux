---
id: C-544
title: "Create a new agent loop by prompting, available in the loop selector"
pillar: Core
status: ready
priority: 32
epic:
design: docs/designs/agent-loop-harnesses.md
areas: [flux-tui, flux-lang, flux-agent]
depends_on: [C-543, C-569]
note: "\"create this <...> loop for me\" -> flux writes the *.flux file; the new loop then appears in the C-543 selector"
---

# Create a new agent loop by prompting, available in the loop selector

## Goal

An operator can create a new agent loop by describing what it should do — "create this <…> new
loop for me" — and flux generates the corresponding `*.flux` file, so new loops are authored
conversationally and become immediately selectable in the TUI loop selector without hand-writing
flux source.

## Acceptance

- [x] A prompt-driven creation path exists ("create this <…> loop for me"): flux generates a
      `*.flux` loop file from the description. *(First pass — confirm the entry point: TUI command,
      chat directive, or CLI subcommand.)* Resolved: the **TUI command** `/loop <what it should
      do>`, beside the C-543 selector it feeds — bare `/loop` opens that selector, exactly as `F3`
      does.
- [x] The generated file is valid flux: it parses and passes the same load-time validation as a
      hand-written loop before it is offered for use; an invalid generation is refused with the
      error surfaced, never silently saved. Failing-first tests for both the accept and refuse
      paths.
- [x] The new loop appears in the [C-543](C-543-tui-loop-selector-with-visualizing-overlay.md)
      selector immediately after creation and can be selected and run.
- [x] The generated loop carries a description (used by the C-543 overlay) derived from the
      operator's prompt.
- [x] The saved loop has stable profile/revision/source-digest metadata for C-569 resolution. Creating
      it does not automatically authorize it for Fleet task kinds or alter an admitted worker.
- [ ] The gate is green in both workspaces.

## Progress

- 2026-08-06 — implemented. `/loop <what it should do>` authors the loop. The surface derives the
  profile name from the description's first meaningful words, generates the documented minimal
  custom loop (`docs/agent-loop.md`) with the description as its leading comment block — the block
  C-543's overlay renders as the loop's description — and the same text bound and observed as the
  loop's `$brief`, so the prompt is part of the program rather than a comment about it. It is then
  admitted exactly as the selector admits a hand-written file: `AgentLoopSpec::parse` plus
  `AgentLoopBinding::native_flux`, which yields the profile, revision `1`, `file:` source reference
  and source digest C-569 resolves against. Only after that does anything reach `.flux/loops`: an
  invalid generation returns its error and writes nothing, and an existing loop file is never
  overwritten. `create_loop_with` keeps *producing* source separate from *admitting* it, so the
  refuse path is proven through the same entry point the accept path uses. On success the selector
  reopens over a fresh scan with the new loop under the cursor, one `Enter` from running. Authoring
  is not an admission: an admitted session still receives C-543's explicit re-admission refusal when
  it selects the new loop, and nothing here touches Fleet task-kind policy.
- Remaining: the full repository gate in both workspaces runs at the wave integration boundary, not
  from the story worktree. The committed public-documentation mirror
  (`crates/flux-server/assets/public-docs.zip`) is rebuilt on the assembled candidate, not here.

## Notes

- Filed 2026-08-05 via /track:story from the roadmap coordinator session; first-pass draft from the
  title — refine the entry point and the generation contract (model-assisted vs template) before
  dispatch.
- Depends on [C-543](C-543-tui-loop-selector-with-visualizing-overlay.md) (the selector and its
  live-refresh acceptance) plus C-569's binding validation. Dispatch serialized after them or in the
  same wave starting from their integrated prerequisite.
