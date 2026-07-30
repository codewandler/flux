---
id: C-220
title: The SurfaceSink contract at L2 — typed pane commands, redacted at the reporter
pillar: Core
status: done
priority: 11
epic: agent-authored-surface
design: docs/designs/agent-authored-surface.md
areas: [flux-runtime]
note: "third instance of a twice-proven pattern — ToolProgressSink/SpawnActivitySink (flux-runtime lib.rs:188-262) are the template: trait at L2, installed by the L6 surface, reached only via ToolContext, redaction applied at the reporter so no tool can put raw bytes on a screen"
---

# The `SurfaceSink` contract at L2

## Goal
Define the tool→surface pane contract in `flux-runtime` so a tool can address the human surface
without any crate below L6 knowing a surface exists. No rendering, no ops — just the types, the
sink, the reporter, and the deny-by-default accessor everything else builds on.

## Acceptance
- [x] `SurfaceSink` (send-only, synchronous, `Send + Sync`) plus `PaneCommand` / `PaneSpec` /
      `PaneData` land in `crates/flux-runtime/src/lib.rs` beside `ToolProgressSink` (`:233-262`),
      following its documented constraints verbatim: implementations must not block and must not
      hold a lock across an await.
- [x] `PaneSpec` carries `id`, `title`, `slot`, `kind`, `lifetime`, `data` and **nothing that reaches
      a `Style`** — no colour, no width, no rect, no z-order. Pinned by a test that the type has no
      style-bearing field, so the trust property C-222 relies on cannot be widened by accident.
- [x] `lifetime: project` parses but is **rejected at the reporter** with a clear error until a story
      claims it — the field exists for forward compatibility, the behaviour does not exist yet.
- [x] `SurfaceReporter` is the only way to reach a `SurfaceSink`, mirroring `ToolProgressReporter`
      (`:248-262`). **Failing-first test:** a registered secret in a pane `title` and in a `data`
      string is delivered redacted — the same guarantee, and the same reason, as the tool-progress
      redaction test.
- [x] `ToolContext::surface()` returns `Option<SurfaceReporter>`, `None` when no host installed a
      sink — the exact posture of `progress_reporter` (`:1042`). Test: a context with no sink yields
      `None`.
- [x] `cargo test -p flux-codegate` green — `flux-runtime` gains no dependency, and certainly not on
      `flux-tui`.

## Progress
- Contract landed in `crates/flux-runtime/src/lib.rs` beside `ToolProgressReporter`: `PaneSlot`,
  `PaneKind`, `PaneLifetime`, `PaneNode`, `PaneData`, `PaneSpec`, `PaneCommand`, `SurfaceSink`,
  `SurfaceReporter`. Storage side mirrors `tool_progress` exactly — a `RuntimeTurnContext::surface`
  field with `with_surface_sink`/`surface_sink()`, a stored `ToolContext::surface` fallback slot
  with `set_surface_sink`, and `ToolContext::surface()` reading the lexical turn first.
- Failing-first test: `pane_content_is_redacted_before_it_reaches_a_surface` (did not compile
  before the change — the contract did not exist).
- Redaction is applied on **one** path (`SurfaceReporter::send`) for all three command shapes, so
  `update`/`close` cannot grow an unredacted route. `id` is redacted too; redaction is
  deterministic, so an open/update/close triple still addresses the same pane.
- Nothing model-facing landed: no op, no catalog change, no prompt-prefix movement, as intended.
- **Added beyond the letter of the Acceptance:** the reporter also rejects a deserialized `PaneSpec`
  whose `kind` contradicts its `data`. `PaneSpec::new` derives `kind` from `data`, so the two can
  only disagree via the wire — which is exactly where C-223 will parse model input.
- Deferred to C-221/C-223 by design: no depth cap on `PaneData::Tree` (redaction recurses over
  model-supplied nesting). The cap belongs at the op's parse boundary, with the other input limits.

## Notes
- Read `crates/flux-runtime/src/lib.rs:188-262` first and copy its shape rather than improvising.
  The doc comments there state the contract's real constraints (non-blocking, no lock across await,
  redaction is non-bypassable) and the reasons for them; this story is a third instance, not a new
  design.
- The `spawn_activity` / `tool_progress` stored-fallback slots on `ToolContext` (`:925-928`) show the
  established shape for the storage side, including why ordinary engine turns carry the sink
  lexically in `RuntimeTurnContext` instead.
- **Nothing model-facing lands here.** No op is registered by this story, so nothing changes in the
  catalog and no prompt prefix moves. That is intentional: the contract and its redaction proof land
  before anything can call it.
