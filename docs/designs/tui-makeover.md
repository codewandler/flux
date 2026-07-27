# Flux TUI makeover

**Story:** [A-65](../stories/A-65-tui-daily-driver-makeover.md)
**Pillar:** Agent · **Layer:** L6 (`flux-tui`, `flux-cli`) with small additive L3/L0 seams

**Status:** implemented and fully gated on 2026-07-10.

## Intent

`flux tui` already streams Markdown, plans, thinking, tool activity, approvals, and usage. Its
screen, however, spends four rows and two columns on permanent boxes; its `/new` command does not
actually create a session; resumed sessions open visually empty; a second queued prompt silently
replaces the first; history performs direct filesystem IO; and the event loop redraws forever while
idle. The makeover keeps the runtime behavior and rebuilds the application/presentation layer.

The visual rule is deliberately strict: the transcript has no frame, and the multiline composer is
separated only by a neutral background. There is no composer border, title, outer margin, or inner
padding. Plans remain fully visible because auditability is a product invariant; thinking and verbose
tool output are secondary and collapse by default.

## Application model

The TUI is an event-driven reducer. Each asynchronous action receives a monotonic action id; late
events from a cancelled or completed action are ignored. The engine lives behind an async `RwLock`:
turns hold a read guard, while idle model switches take a write guard. A visible FIFO owns follow-up
messages; it drains only after the active task has joined and pauses while its editor is open.
Editing keeps the selected item in its original slot until submit; an action that finishes meanwhile
cannot drain past or promote that item.

Terminal input uses crossterm's async event stream. A guard records raw mode, alternate screen,
mouse capture, bracketed paste, and cursor state as each is enabled and unwinds the successful subset
in reverse order. Rendering occurs only after state changes or while an animation is active. The
cell-width-wrapped transcript layout is cached, and only the selected viewport rows are handed to
Ratatui on a draw.

## Durable reconstruction

Selecting or resuming a session folds its ordered event stream. Messages, accepted plan attempts,
real leaf-op trace cells, displayed observations, model changes, and turn usage become transcript
entries and metrics. Compaction is shown as a context boundary but never duplicates the compacted
snapshot. Projection is read-only and cannot dispatch.

`RunEvent::OpRecorded` gains optional `input_view` and `input_view_truncated` fields. The recorder
decodes JSON string leaves before redaction (so quotes, backslashes, and newlines cannot evade the
registered-secret matcher), then caps the presentation on a valid character boundary before append.
The field is
display-only: replay matching continues to use the existing hashes, and result replayability
continues to use the existing `truncated` flag. Older records lack the field and render a reduced
tool header without inventing an argument. Historical risk is never recomputed from today's registry.
Sessions recorded before cassette cells existed, or with cassette capture disabled, reconstruct
reduced completed/failed cards from `StepStarted` plus its terminal event; loop machinery uses the
same central filter as the live sink and remains hidden.

Durable activity intentionally excludes raw thinking, animation frames, transient approval panels,
and loop/composite presentation wrappers. Those are UI state rather than execution evidence.

## Commands and safety

The TUI owns `/help`, `/quit`, `/plan`, `/run`, `/model`, `/effort`, `/shell`, `/tools`, `/evidence`,
`/session`, `/sessions`, `/resume`, `/new`, `/clear`, `/compact`, and `/queue`. `/new` and `/clear`
both mint a real session; `/sessions` is a dense keyboard picker. State-changing commands require an
idle action; quitting denies a pending approval, cancels, clears queued work, joins, then restores the
terminal. Reviewed `/run` execution uses one shared `FlowEngine` helper so the CLI and TUI retain the
same approved-plan scope and undisclosed-destructive re-prompt behavior.

Persistent prompt recall is derived from event-store user messages, removing the TUI's direct
`~/.flux/history` IO. The CLI persists new `always allow` rules after the TUI returns, even on an
error path. Empty-session pruning uses an atomic keep-list for the active session, `/model mock`
resolves to the credential-free provider, and session projection retains the engine model that will
actually execute the next turn rather than replacing it with historical registry metadata.

## Verification

Reducer and projection behavior is unit-tested without a terminal. `TestBackend` pins cell content
and style at ordinary, narrow, and too-small dimensions. Engine tests cover model switching,
reviewed-plan cancellation/turn-end behavior, event compatibility, and redaction. A real PTY
mock-provider smoke verified paste, queue, approval, session creation/resume, and restoration. The
full workspace build/test/clippy/fmt gate and layering lint passed.
