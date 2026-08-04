---
id: C-527
title: "ui.display — show the user a file, diff, flow or artifact by reference"
pillar: Core
status: backlog
priority: P1
epic: agent-authored-surface
design: docs/designs/agent-authored-surface.md
areas: [flux-runtime, flux-tools, flux-tui, flux-cli]
depends_on: [C-220, C-221, C-222, C-305]
note: "The agent should point at what it already made; it must not spend context reconstructing a file, diff or flow merely to show it to the user"
---

# `ui.display` — show the user a file, diff, flow or artifact by reference

## Goal

Give an attached Flux UI one Core operation the agent can call when it wants the user to look at an
artifact: `ui.display`. The call carries a guarded workspace path or an opaque reference to content
that already exists—never another model-authored copy of that content. The runtime resolves the
reference once, determines its authoritative type, and lets the surface present a file, Markdown,
code, image, diff, Flux flow/tree or future media kind through its native renderer.

This is the reference-first counterpart to `pane.open`. Panes remain useful for model-authored live
status. `ui.display` is for “look at the thing I just created” without making the model remember,
truncate or regenerate the thing inside a tool argument.

## Acceptance

- [ ] Amend the agent-authored-surface design before implementation. `ui.display` reuses the one
      `SurfaceSink`/trusted-chrome path established by C-220…C-222; it does not create a second UI
      callback, terminal renderer or unmarked overlay. The host alone chooses placement, geometry,
      theme, renderer, focus and lifetime, and approval/trusted overlays still draw above every
      agent-requested display.
- [ ] The public operation is named exactly `ui.display`. It is registered once at assembly only
      when the host installed a display-capable surface, following C-305's sink-presence rule.
      Headless `flux run`, server and SDK assemblies without that capability neither advertise nor
      silently accept it. Surface capabilities are typed and immutable for the assembled catalog so
      renderer availability cannot churn the model's prompt prefix mid-session.
- [ ] The input is reference-first and deny-unknown. It accepts exactly one source from a closed
      union: a workspace-relative guarded `path`; an immutable host-issued `content_ref`; a stored
      `flow_ref`; or an immutable `diff_ref`. Optional fields are a plain title and a presentation
      hint. There is no inline text, byte/base64 payload, URL, arbitrary cwd, filesystem root,
      renderer command, HTML, style, geometry or executable field. Supplying two sources, an unknown
      reference kind or an expired reference is a repairable refusal.
- [ ] A shared L2 `DisplayRef`/`DisplayRequest` vocabulary gives every reference a bounded safe id,
      producing session, immutable content identity/digest, authoritative media type and optional
      safe display name. References contain no content, credential, bearer, absolute host path or
      external URL. They are unforgeable opaque capabilities scoped to the session/owner that
      produced them; another session, project or principal cannot resolve one by guessing its id.
- [ ] Tools that naturally produce displayable artifacts can return a `DisplayRef` in structured
      result metadata without changing their ordinary model-readable summary. At minimum the
      guarded edit/write path can identify the resulting file snapshot, Git diff/merge-tree output
      can identify an immutable diff snapshot, and `flow_render`/the stored-flow catalog can identify
      a flow. The model receives the small reference and descriptive metadata, never a duplicate
      hidden payload it must echo into `ui.display`.
- [ ] A `path` is resolved at call time through the same workspace-pinned guarded `System` read path
      as native file tools. It rejects absolute paths, `..`, option-shaped paths, symlink/reparse
      escapes, devices, sockets and files outside the admitted root before opening. The operation
      declares the real read effect and concrete permission subject; `ui.display` is not a bypass
      around file-read approval merely because the destination is the local user's screen.
- [ ] Reference resolution is centralized and snapshot-safe. An immutable content/diff reference
      either resolves to the exact recorded digest or refuses; it never silently displays newer
      path contents. A direct path deliberately displays the current guarded file and reports its
      resolved digest. Resolution happens once—metadata sniffing and rendering consume the same
      opened object/bytes so a path swap cannot create a check/use race.
- [ ] Media type is derived from trusted producer metadata plus bounded byte inspection. A caller's
      presentation hint may choose among compatible views (for example source versus tree for a
      Flux flow) but cannot relabel arbitrary bytes as active HTML, an image, a diff or another less
      restricted type. Unknown/binary media gets a safe metadata/hex-or-download fallback chosen by
      the host; no renderer executes scripts, follows external references, loads network resources
      or invokes an external program.
- [ ] The first renderer family covers plain/Markdown text, source code with language metadata,
      unified and structured Git diffs, images supported by the attached surface, and Flux source/
      tree views by reusing `flux-markdown`, the existing diff view and `flow_render` machinery.
      Capability negotiation is explicit: an unsupported image/renderer produces a visible typed
      fallback or refusal rather than “displayed” while drawing nothing. The media-type dispatch is
      extensible without adding another argument shape or renaming the operation.
- [ ] Resolution and projection are hard-bounded by bytes, lines, dimensions, nesting and render
      time. Text passes the shared redactor and terminal-control/bidi sanitizer before the surface
      can hold it; binary projections never enter model context or session transcript. Truncation is
      explicit with original byte count and digest, and the UI offers a user-driven guarded open/
      export action when available instead of letting the model raise the bound.
- [ ] The operation's result is terse structured metadata—`displayed`, resolved reference/digest,
      media type, renderer and `complete|truncated|unsupported`—with no artifact content. Display
      requests/outcomes are auditable without persisting the payload or turning the transcript,
      event log, diagnostics or tool result into a second copy. Repeating the call must still reach
      the surface; it is never swallowed by the idempotent operation cache.
- [ ] Failing-first end-to-end tests drive a scripted model through the real TUI assembly and prove:
      a newly written file displays without its bytes appearing in the `ui.display` call; a recorded
      diff remains byte-identical after the worktree changes; a stored flow selects source/tree
      views; Markdown/control-sequence adversaries remain inside marked agent chrome; an escaped
      path, forged/cross-session ref, MIME mismatch, oversized object and unsupported renderer all
      refuse or fall back truthfully; and a headless catalog contains no `ui.display`.
- [ ] The operation catalog, in-repo reference, help and website explain when to use `ui.display`
      versus `pane.open` or an ordinary final answer. Standard workspace build/test/clippy/fmt,
      `flux-codegate`, generated-reference checks, `scripts/build-embedded-docs.sh`, its `--check`,
      embedded-doc gates and website tests/build are green before the PR.

## Progress

- 2026-08-04 — filed from a direct request for an agent operation that can show “all kinds of
  things”—especially a file, generated diff or Flux flow—by path/reference instead of regenerating
  the content from model memory.

## Notes

- Existing presentation boundary: `flux-runtime::{SurfaceSink, SurfaceReporter, PaneCommand}` and
  `flux-tui/src/{panes,trust}.rs`. Extend that one marked agent surface; do not route around it.
- Existing model-facing precedent: `crates/flux-tools/src/surface.rs` conditionally registers
  `pane.open|update|close` from sink presence. `ui.display` needs its own typed capability bit because
  a pane sink that can draw bounded text is not automatically able to render every media family.
- Existing reference/rendering seams: `flux-lang::ThingRef`, the stored-flow catalog and
  `crates/flux-tools/src/render.rs`; Git diff types in `flux-events::projection`; guarded workspace
  IO in `flux-system`. Prefer one narrower `DisplayRef` over widening generic `ThingRef` into a
  bearer or teaching each renderer to open paths independently.
- “Reference-first” is load-bearing. An inline-content escape hatch would immediately recreate the
  context waste and stale-copy problem this story exists to remove.
