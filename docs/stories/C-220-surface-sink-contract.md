---
id: C-220
title: The SurfaceSink contract at L2 — typed pane commands, redacted at the reporter
pillar: Core
status: ready
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
- [ ] `SurfaceSink` (send-only, synchronous, `Send + Sync`) plus `PaneCommand` / `PaneSpec` /
      `PaneData` land in `crates/flux-runtime/src/lib.rs` beside `ToolProgressSink` (`:233-262`),
      following its documented constraints verbatim: implementations must not block and must not
      hold a lock across an await.
- [ ] `PaneSpec` carries `id`, `title`, `slot`, `kind`, `lifetime`, `data` and **nothing that reaches
      a `Style`** — no colour, no width, no rect, no z-order. Pinned by a test that the type has no
      style-bearing field, so the trust property C-222 relies on cannot be widened by accident.
- [ ] `lifetime: project` parses but is **rejected at the reporter** with a clear error until a story
      claims it — the field exists for forward compatibility, the behaviour does not exist yet.
- [ ] `SurfaceReporter` is the only way to reach a `SurfaceSink`, mirroring `ToolProgressReporter`
      (`:248-262`). **Failing-first test:** a registered secret in a pane `title` and in a `data`
      string is delivered redacted — the same guarantee, and the same reason, as the tool-progress
      redaction test.
- [ ] `ToolContext::surface()` returns `Option<SurfaceReporter>`, `None` when no host installed a
      sink — the exact posture of `progress_reporter` (`:1042`). Test: a context with no sink yields
      `None`.
- [ ] `cargo test -p flux-codegate` green — `flux-runtime` gains no dependency, and certainly not on
      `flux-tui`.

## Progress
- (not started)

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
