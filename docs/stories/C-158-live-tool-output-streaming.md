---
id: C-158
title: Stream partial tool output onto running tool cards
pillar: Core
status: blocked
epic: tui-polish-round-2
design:
note: "BLOCKED on a boundary decision, not on effort — the 2026-07-29 investigation (see Progress) proved no in-flight *content* is observable from flux-tui/flux-cli/flux-runtime/flux-core: bash/task are awaited as one opaque unit from other crates, and the one live relay that does reach a running op deliberately carries no content field. Unblocking means deciding to source progress at the tool (flux-tools/flux-orchestrate/flux-system) and, for sub-agents, deliberately loosening SpawnActivityEvent's content boundary; the install seam is already confirmed"
---

# Stream partial tool output onto running tool cards

## Goal
A long `bash` or `task` call shows an animated `◌ running` badge with live elapsed (C-109,
`lib.rs:1793-1799`) but no content: the summary line only renders once `tool.result` is `Some`
(`lib.rs:1812-1826`). For multi-second ops the user cannot tell whether the op is progressing or
stuck. Show the last line (or a bounded tail) of in-flight output under the header.

## Acceptance
- [ ] A running tool card renders a bounded, redacted tail of partial output that updates as the op
      runs, and is replaced by the normal summary/detail when the result lands — failing-first test
      driving partial output through the entry pipeline.
- [ ] Partial output flows through the same redaction the final result gets; nothing bypasses the
      guarded/redacted result path.
- [ ] The C-109 badge patch and the running-row pairing (`lib.rs:1554-1567`) still hold with the
      extra row present.
- [ ] Ops that produce no incremental output render exactly as today (no empty placeholder row).

## Progress
- 2026-07-29: **STOPPED — not implemented.** Traced the full pipeline from a running op to a TUI
  entry and found a hard architectural wall inside the crates this run was scoped to (`flux-tui`,
  `flux-cli`, `flux-runtime`, `flux-core`): there is no point in that set where genuine in-flight
  *content* from a running op is observable, so any "partial output" seam built only inside them
  would be permanently unfed — dead plumbing, not a feature. Evidence, traced concretely:
  1. **Every built-in `Tool` impl with real IO lives outside the allowed set.** `BashTool`
     (`crates/flux-tools/src/lib.rs:1080`) is in `flux-tools`; the sub-agent `task` op (`TaskTool`)
     is in `flux-orchestrate`. Neither crate is in scope. `flux_runtime::Executor::dispatch`
     (`crates/flux-runtime/src/lib.rs:3519-3520`, allowed) calls `tool.execute(&self.ctx, params)`
     and `.await`s the whole future as one opaque unit — it has no visibility into what the tool
     does while that future is pending, and redaction (`crates/flux-runtime/src/lib.rs:3524`,
     `self.ctx.redactor.redact(&r.content)`) only ever runs once, on the fully-resolved result.
     There is no mid-flight hook to add on the `flux-runtime` side of that `.await` without the
     *tool* cooperating from inside `flux-tools`.
  2. **The one primitive that supports polling a running process's output** —
     `flux_system::System::spawn_background` → `ManagedChild::read_output()`
     (`crates/flux-system/src/lib.rs:900-921`, continuously-drained buffers) — lives in
     `flux-system`, also out of scope, and isn't what `bash` uses anyway: `BashTool::execute`
     calls `ctx.system().run(...)` → `run_with_env` → `run_with_env_confinement`
     (`crates/flux-system/src/lib.rs:1741-1776`), which pipes stdout/stderr, spawns, and
     `await_process`s straight through with **no buffer or handle exposed to the caller before it
     resolves**. Nothing to poll even if `flux-system` were in scope.
  3. **The one existing live cross-boundary relay that already reaches a running op** —
     `SpawnActivitySink` (`crates/flux-runtime/src/lib.rs:191-192`, defined in the allowed
     `flux-runtime`, already wired end-to-end today by `flux-orchestrate`'s `TaskTool` and
     `flux-flow`'s `loop_host.rs` with zero changes needed) forwards a **running** sub-agent's own
     tool calls up to the parent's `AgentSink.observation()` live, per-event, not batched at the
     end. It looked like a real seam. But `SpawnActivityEvent::ToolResult` deliberately carries
     only `{ call_id, name, is_error }` — **no content field** — per its own doc comment: "Tool
     result *content* is intentionally absent; a customer-facing surface must still default-deny
     the tool input and observation data carried by this trusted host-side contract"
     (`crates/flux-runtime/src/lib.rs:129-131`). That's a deliberate security boundary (keeping a
     spawned child's raw output from crossing to the parent's surface unredacted-by-construction),
     not an oversight — widening it to carry content is exactly the kind of "add a bypass path"
     AGENTS.md rules out, and is a materially different, larger decision than this story.
  - Net: sourcing real, redacted, in-flight *content* requires editing at least one of
    `flux-tools` (make `BashTool` report progress), `flux-orchestrate` (make `TaskTool` forward
    real content, which also means deliberately loosening the `SpawnActivityEvent` content
    boundary above), or `flux-system` (expose a pollable buffer on the synchronous `run` path) —
    none of which are in this run's allowed crate set. The story's own fallback ladder is "largest
    honest subset (e.g. bash/process ops only) … if even that is impossible, STOP and report why
    instead of forcing it" — bash and task (the two ops the story's own note names) are both
    unreachable, so this hit the STOP condition rather than the subset condition.
  - What *is* confirmed reachable, for a follow-up: `flux-cli`'s `assemble_cli_execution_environment`
    (`crates/flux-cli/src/execution.rs:879-897`) is the real production seam that builds the
    `ExecutionEnvironment`/`ToolContext` for both the plain-CLI agentic path and the TUI/REPL
    (confirmed by tracing `build_agent`/`build_agent_with` and `AgentSpec::into_engine`, which
    takes an already-built `Executor` from the caller rather than constructing one itself) — so a
    future story that's *also* allowed to touch `flux-tools`/`flux-orchestrate`/`flux-system` has
    a clean installation point for a new `ProgressSink`-style capability (mirroring the existing
    `SkillLoader`/`LoopHost` pattern on `ToolContext`) without needing to touch `flux-agent` or
    `flux-flow` for the installation half — only for sourcing the content itself, at the tool.
  - No source files touched for this story. No tests added (nothing to pin — there is no seam to
    test against without a real producer, and a synthetic test exercising an event that no
    production code path can ever emit would be theater, not coverage).

## Notes
- Correction recorded during review: the original "dead air" framing was wrong — motion already
  exists. This story is about content, and it is deliberately last in the epic because it needs
  incremental output to reach the TUI entry pipeline before `tool.result` is set; that is an event
  path change, not a rendering change.
- Sequence after C-149 so the card layout is settled; otherwise independent of the other stories.
