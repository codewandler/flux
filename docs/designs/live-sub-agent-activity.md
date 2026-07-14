# Live, correlated sub-agent activity

**Status:** implemented ([A-79](../stories/A-79-stream-correlated-sub-agent-activity.md)) · **Layer:**
L2 reporter contract (`flux-runtime`) + L3 engine/orchestration wiring (`flux-flow`,
`flux-orchestrate`)

## Problem

`LocalSpawner` runs a child `FlowEngine` with a private `TextCollector`. That collector preserves the
child's final prose and counts tool calls, but drops planning brackets, progress observations and the tool
lifecycle. A parent chat can therefore say that it delegated and later show the answer, but remains silent
for the whole specialist run.

Teeing child calls into the parent's ordinary `AgentSink::tool_call(name, input)` is not sufficient. Those
callbacks do not identify the role or child session, so two specialists using the same operation cannot
pair their outcomes. Forwarding child prose or thinking would also duplicate answers and violate the
surface privacy boundary.

## Contract

`flux-runtime` owns a small, synchronous `SpawnActivitySink` callback and structured
`SpawnActivity`/`SpawnActivityEvent` values. The value carries:

- role, child session and parent session correlation on every event;
- balanced planning state;
- tool call identity, name and input for a trusted host-side projector;
- timing plus success/error state, never tool-result content;
- redactor-scrubbed observations; and
- exactly-once child completion/usage with a success/failure bit from the spawner boundary.

Thinking deltas and text deltas are deliberately absent. Tool input and observation data remain an
internal sink contract: a customer surface must default-deny them and derive fixed or explicitly
allowlisted labels. The child redactor scrubs registered secrets from both JSON keys and values before
either reaches the reporter.

`SpawnRequest` carries the optional reporter. `ToolContext` stores an explicitly installed reporter in the
same per-turn, one-active-turn slot pattern as cancellation and session correlation.
`EngineLoopHost::set_turn` snapshots the owned channel sink into an L3 adapter implementing the L2
reporter; `TaskTool` copies that snapshot into the request. The adapter encodes the typed value as the
reserved `subagent.activity` observation, using `AgentSink`'s existing extension point instead of adding an
uncorrelated child callback to that public trait.

Adapter tools sometimes open a second runtime inside their guarded `execute` future. The executor scopes
that future with a Tokio task-local view of the reporter; a nested `ToolContext::spawn_activity_sink()` can
therefore snapshot the same reporter while the adapter is active. The scope is concurrency-safe and
lexical: a nested context retained after the adapter returns cannot keep an obsolete turn callback. This
is what preserves live reporting through `parent FlowEngine -> adapter tool -> one-shot FlowClient ->
TaskTool`, not only through a directly registered parent `TaskTool`.

`FlowClient::build_executor` pins that lexical snapshot onto its fresh context before
`execute_streamed` moves the executor into a spawned Tokio task; Tokio task-locals themselves do not
cross that boundary. Shared/cloned context slot restoration belongs to A-80 together with cancellation
and session lineage; the supported adapter path constructs a fresh one-shot context.

## Forwarding path

```text
surface AgentSink::observation("subagent.activity")
  ↑ parent ChannelSink ← AgentSinkSpawnActivitySink (snapshotted for this turn)
  ↑ SpawnRequest.activity (explicit or lexically inherited through an adapter tool)
LocalSpawner child collector
  ├─ collect final text + tool count privately
  └─ emit correlated planning/tool/observation/completion activity
```

The child collector assigns a monotonically increasing call id and maintains a per-name pending stack.
Role + child session is the cross-child correlation key; the call id pairs a result within that child.
When bounded nested delegation is enabled, `subagent_activity` from a grandchild is relayed unchanged by
the intermediate collector, preserving the originating role/session/call identity.

## Invariants

- Child text remains the `SpawnOutcome.text` returned to `task`; it is never streamed as parent prose.
- Child thinking never leaves the child sink.
- Tool result/error content never enters `SpawnActivity`; only `is_error` and timing cross.
- Completion fires once at the spawner boundary. Engine errors, deadlines, cancellation, and a dropped
  in-flight spawn report failure without forwarding error text.
- Parent cancellation drops the child-owning flow future before the parent's final activity-channel
  drain, so the collector's synchronous drop-time completion reaches the surface before teardown.
- Every tool still executes through the child's `Executor`; forwarding is observational only.
- Reporter callbacks are synchronous send-only and hold no lock across an await.
- Fresh nested-context reporter inheritance exists only while the outer guarded tool future is executing;
  there is no process-global reporter, and streamed one-shot execution pins only that lexical snapshot.
- Reporter absence is a strict no-op, preserving every existing CLI/SDK consumer.
- Cancellation, deadlines, usage roll-up, correlated audit streams and side-channel UI block collectors
  keep their existing ownership and behavior.

## Test boundary

The failing-first regressions cover each boundary: a child status + read is visible while the read remains
blocked; a real parent engine derives and snapshots its reporter; a guarded adapter's nested context sees
the reporter only inside the lexical execution scope; a streamed nested runtime pins it across
`tokio::spawn`; concurrent storeless children have distinct spawn ids; JSON keys and values are scrubbed;
and a timed-out child emits one failure terminal. A parent-cancellation regression holds a real `task`
collector pending, cancels the parent, and requires its single failure completion on the borrowed surface.
Downstream, a projector unit interleaves same-named calls by child identity, while the served-chat
regression drives the complete nested-`FlowClient` manager route. Existing cancellation, usage and audit
tests remain the regression net.
