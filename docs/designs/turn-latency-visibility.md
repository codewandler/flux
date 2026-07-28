# Design: turn latency visibility — where the wall clock actually went

**Status:** proposed 2026-07-28 · **Pillar:** Core · **Stories:** [C-180](../stories/C-180-tui-llm-wait-time.md), [C-181](../stories/C-181-provider-retry-observer.md), [C-182](../stories/C-182-plan-approval-sheet-ops.md)

## Why

The TUI attributes execution time for **operations** and nothing else. Every tool card carries
`exec 1.2s` (+ `approval 30s` when a human sat on the gate), and the footer closes a turn with
`4 steps · 18.1s`. But an agent turn is mostly *not* op execution — it is model inference, and none
of that time is attributed anywhere on the surface. A turn that reads `18.1s` gives no way to tell
apart "the model thought for 16s" from "one `bash` op took 16s" from "the provider 429'd and we
slept through four exponential backoffs".

The measurements already exist; they are dropped on the floor:

- `ModelCallMetrics` (`crates/flux-flow/src/model.rs:32`) captures `duration_us`, `ttft_us`, and
  `chunks` at the provider-stream boundary, and `observe_model_call`
  (`crates/flux-flow/src/staged.rs:2231`) publishes them on the `model.call` observation. The plain
  CLI renders them behind `--trace-loop` (`format_model_call`, `crates/flux-cli/src/rendering.rs:382`).
  The TUI's sink receives the same observation and extracts **only** `usage`/`model`/`stage`/
  `operations` (`crates/flux-tui/src/controller.rs:188`).
- Retries live in `NativeProvider::stream`'s connect loop
  (`crates/flux-provider/src/lib.rs:775-844`) and surface **only** as `tracing::warn!`. No surface
  installs a tracing subscriber, so they are invisible in every product path. `ModelTrace` counts
  `http_attempts` / `oauth_refreshes` / `transport_fallback`, but only prints to stderr under
  `FLUX_MODEL_TRACE`, which is a developer flag and would garble the TUI anyway.

A third, adjacent blind spot shares the same root cause — **surfaces discard structured detail the
engine already computed**. Whole-plan approval hands the approver a `PlanApprovalRequest` carrying
`ops: Vec<String>` and typed `requirements` (`crates/flux-runtime/src/lib.rs:1698`). The plain CLI's
`plan_prompt` (`crates/flux-cli/src/session.rs:1184`) renders both. The TUI's `ChannelApprover`
implements only `Approver::request`, so it falls through to the default `request_plan`
(`crates/flux-runtime/src/lib.rs:1736`), which collapses the whole batch to
`request("run plan", &["3 op(s) · low · mutating"])`. The user is asked to authorize three
operations without being told which three.

None of this is a missing measurement. It is a missing wire.

## Approach

Three independent stories, in dependency order (the retry seam feeds the TUI badge). No new
crates; one new public seam in L1.

> Numbering note: this epic was first drafted as C-168…C-170 and renumbered to C-180…C-182 after a
> concurrent session claimed those IDs for its gateway/codec epic.

### C-181 — a retry observer seam in `flux-provider`

The connect loop is the only place that knows a retry is happening, and it is L1 with no sink. Give
it a narrow, injected observer following the established `scope_runtime_turn` idiom
(`crates/flux-runtime/src/lib.rs:457`):

```rust
pub enum RetryReason { Status(u16), Transport(String), OauthRefresh, TransportFallback }
pub struct RetryEvent { provider, model, attempt, max_attempts, delay, reason }
pub trait RetryObserver: Send + Sync { fn retrying(&self, event: &RetryEvent); }

pub async fn with_retry_observer<F: Future>(observer: Arc<dyn RetryObserver>, fut: F) -> F::Output;
```

backed by a `tokio::task_local!`. `NativeProvider::stream` notifies **before** each backoff sleep, so
the event lands while the wait is still ahead of the user — the whole point. A missing observer is a
no-op, so every embedder and every test keeps working untouched.

The model stage (`crates/flux-flow/src/staged.rs`) wraps its `stream_blocks` call in the scope with
an observer that does two jobs: forwards a `model.retry` observation to the live `AgentSink`, and
counts. The counts fold into `ModelCallMetrics` and out onto the `model.call` observation, so the
after-the-fact figure survives even when the call ultimately fails (the stream never exists on that
path, so it cannot carry the count itself).

Why a task-local rather than a field on `NativeProvider` or `Request`: providers are built once per
session and shared, while the sink is per-turn — a field would need a mutable shared slot updated on
every turn, which is exactly the ambient-state seam this codebase avoids elsewhere. A task-local is
lexically scoped to the one model call, isolates concurrent turns by construction, and restores
automatically on nesting.

### C-180 — spend the measurements in the TUI

Three displays over one data path (`model.call` → `UiEvent` → `ChatState`):

- **Per-call badge.** The `model.call` observation arrives *after* `Planning(false)` seals the
  thinking entry (`staged.rs:721-737` — the `PlanningGuard` drops at the end of the block, the
  observation is emitted on the next line), so the badge can be attached to the just-sealed
  `Entry::Thinking` without racing the sealing logic. Renders as one dim line:
  `◇ model stage.explore #2 · 4.2s · ttft 0.9s · ↻ 2 retries`. Attaching it to the thinking entry
  rather than pushing a standalone entry keeps it visible even for a stage with no thinking tokens
  (the entry is created on `Planning(true)` regardless) and costs no extra entry separator.
- **Live model timer.** `Planning(true)` stamps `model_call_start`; the footer's running arm adds
  `· model 3.2s` beside the turn elapsed, answering "is it the model or an op that is slow *right
  now*". A live retry replaces it with a warn-styled `↻ retry 2/6 · 4s`.
- **Turn aggregate.** Accumulate each call's `duration_us` across the turn; the closing footer
  segment becomes `4 steps · 18.1s · llm 12.4s`.

### C-182 — render what plan approval already carries

Implement `Approver::request_plan` on the TUI's `ChannelApprover`, building the sheet's content from
the request the same way `plan_prompt` does: risk summary, destructive warning, the op names, and
the concrete statically-visible targets (typed `requirements` minus `Operation`-kind duplicates,
plus `IntentTarget::Process` commands). The existing sheet already windows and scrolls its subject
list, so the detail lines reuse that machinery; `ApprovalView` gains a risk `summary` and a
`destructive` flag so the header can carry the badge in risk color.

This changes **display only**. Approval semantics, the receipt binding, and dispatch's re-check of
every op are untouched — an undisclosed destructive op still re-fires the per-op gate inside an
approved scope, exactly as before.

## Non-goals

- No new telemetry store or metrics backend. Everything rides existing observations.
- No retry-policy changes. `DEFAULT_MAX_RETRIES`, the backoff curve, and `Retry-After` handling stay
  exactly as they are; this only makes them visible.
- No plain-CLI redesign. The CLI already renders both the model-call line (behind `--trace-loop`) and
  the full plan prompt; it gains the retry counter for free via the shared observation.

## Risks

- **Task-local reach.** `with_retry_observer` only covers work polled on the same task. The model
  stage awaits `stream_blocks` inline, so this holds; a future provider that moves its connect into
  a detached `tokio::spawn` would silently lose the events. Covered by a test that drives a retrying
  transport through the real scope.
- **Badge noise.** One extra dim line per model round. Kept to a single line and folded into the
  existing thinking entry rather than a new entry, so density matches the existing
  `thinking · N lines` row.
