---
id: C-290
title: "A runtime has no memory ceiling and no general concurrency limit"
pillar: Core
status: in-progress
priority: 4
areas: [flux-config, flux-runtime, flux-sdk]
note: "surveyed while designing the flux-connectors interop: context_budget/max_iterations/max_tokens/max_calls exist, but nothing bounds memory and the only concurrency control is server-side max_inflight_per_principal — so an embedding host cannot bound a runtime it constructs"
---

# A runtime has no memory ceiling and no general concurrency limit

## Goal

Let a host that constructs a runtime bound its resource use, not just its token spend.

## Acceptance

- [x] A host can set a **concurrency limit** when building a client — a ceiling on simultaneously
      executing tool calls — and it applies to in-process embedding, not only to `flux-server`.
      → `ResourceLimits::with_max_concurrent_tool_calls` (`crates/flux-runtime/src/limits.rs`),
      enforced in `Executor::dispatch_outcome` step 4⅝ — the one funnel every in-process tool call
      already traverses.
- [x] A host can set a **memory ceiling**, or this story records precisely why that is not
      implementable in-process and narrows to what is: a bound on the things that actually grow
      without limit (retained tool results, evidence log, transcript).
      → **Narrowed** to `ResourceLimits::with_max_retained_result_bytes`, a byte bound on *retained
      tool results*. See "What the memory half narrowed to" below.
- [x] Whatever lands is reachable from `ClientBuilder` (`crates/flux-sdk/src/lib.rs:371`) alongside
      the existing `context_budget`, `max_iterations`, `max_tokens` and `max_calls`, and from
      `flux-config` for a file-configured host.
      → `ClientBuilder::resource_limits`, `FlowClientBuilder::resource_limits`, and
      `[limits] max_concurrent_tool_calls / tool_call_queue_timeout_ms / max_retained_result_bytes`
      read through `ResourceLimits::from_config`. **Not** wired into the `flux` binary itself —
      `flux-cli` was fenced for this story (C-213 in flight); that wiring is owed separately.
- [x] **Failing-first test:** a runtime configured with a concurrency limit of N never has more than N
      tool executions in flight, demonstrated with a tool that blocks until released.
      → `crates/flux-sdk/tests/resource_limits.rs::a_concurrency_limit_caps_simultaneous_tool_executions`
      (plus the `parallel`-flow variant, which is where an authored flow actually produces
      concurrency, and a shared-budget variant across two executors).
- [x] Exceeding a limit is an observable, actionable refusal — never a silent truncation or a hang.
      → a saturated ceiling refuses after a bounded `tool_call_queue_timeout` (default 30s, no
      "wait forever" setting) with a message naming the limit and the knob, plus a
      `tool_concurrency_refused` observation and a `tool.concurrency_refused` dispatch event.
      Retained-byte pressure *evicts from a cache*, which is correctness-neutral — it never
      truncates a result the model sees.
- [x] The gate is green.

## Notes

- **What exists today**, surveyed rather than assumed: `AgentConfig::max_iterations`,
  `max_model_calls`, `ModelStageConfig::max_tokens`, `ConsultConfig::max_calls`,
  `Limits::turn_token_budget`, and `ServerConfig::max_inflight_per_principal` — that last one being
  the *only* concurrency control, and it is server-side and per-principal.
- So an embedding host today can bound how much a runtime *spends* but not how much it *uses*. That
  asymmetry is the whole of this story.
- The memory half may well be the wrong shape as stated. A process-wide RSS ceiling is not something a
  library can honestly enforce; bounding the specific retained structures is. Prefer narrowing the
  acceptance to something true over shipping a knob that does not bind.
- Raised by the flux-connectors interop design (`docs/designs/connector-tool-pack.md` in that repo).
  Nothing in that work depends on this — it is filed so the gap is recorded rather than rediscovered.

## What the memory half narrowed to

The Notes were right: `max_memory_bytes` is the wrong shape, so it was **not** shipped. A Rust
library cannot observe or refuse an allocation made by its caller, by the provider SDK, by a plugin
subprocess, or by the allocator's arenas, and it cannot unwind a `Vec` growth that already
succeeded. A knob reporting "you are protected to N bytes" while doing none of that is worse than
no knob — it tells an embedding host it is protected when it is not.

Of the three structures the acceptance named, exactly one is both **owned by the runtime** and
**bounded correctness-neutrally**:

| Candidate | Verdict |
| --- | --- |
| **Retained tool results** — `Executor::op_cache` | **Bounded.** It was capped at 512 *entries* and unbounded in bytes: 512 large file reads retained an arbitrary number of them. `max_retained_result_bytes` adds a byte ceiling; overflow clears the cache. A miss re-runs the op, so eviction is invisible to correctness and is not a truncation. |
| **Evidence log** — `flux_evidence::EvidenceLog` | Not bounded. It drives reactions, `metrics()`, and the audit trail. Dropping the oldest observations to fit a ceiling is a silent truncation of an audit record — precisely what this story's acceptance forbids. Bounding it needs a design (spill/summarize), not a knob. |
| **Transcript** | Not bounded here — it is owned by the session store and already has two explicit host-set ceilings: `ClientBuilder::with_compaction` (summarize older turns) and `ClientBuilder::context_budget` (inline knowledge blocks). |

## Progress

**2026-07-30 — landed on `impl/C-290`.**

- `crates/flux-runtime/src/limits.rs` (new): `ResourceLimits`, `ConcurrencyRefusal`, the
  byte-bounded `OpCache`, and the `HELD_SLOTS` task-local. The concurrency ceiling is an
  `Arc<Semaphore>` inside `ResourceLimits`, so **clones share the budget** — that is what makes it a
  runtime ceiling rather than a per-executor one, and it is what keeps `FlowClient::build_executor`
  (a fresh executor per run) from escaping it.
- `HELD_SLOTS` keys on the *semaphore's identity*, not a bare flag: a nested dispatch from inside a
  running tool is exempt from the ceiling it is already counted against (a deadlock at N=1), while a
  nested call against a *different* runtime's ceiling still queues. Tokio task-locals scope to the
  polled future, so sibling `parallel` branches — which `join_all` on one task — never see each
  other's entries.
- The slot is taken **after** the approval gate (a slot must never be held while a human is being
  asked) and **before** the execution, and released the moment the execution ends. A cache hit takes
  no slot: replaying a stored result is not an execution.
- The refusal is deliberately **not** `DispatchOutcome::denied` — it is transient, so `retry`/`loop`
  should try it again, unlike an authorization refusal.

**Owed, deliberately not done here:** wiring `[limits]` into the `flux` binary. `flux-cli` was
fenced for this story. Until that lands, the `[limits]` concurrency keys are consumed only by an
embedding host via `ResourceLimits::from_config`; the field docs in `flux-config` say so.

**Published-API note for the release cut:** this is **additive** to `flux-sdk`'s public surface
(`ClientBuilder::resource_limits`, `FlowClientBuilder::resource_limits`, `Client::resource_limits`,
the `ResourceLimits`/`ConcurrencyRefusal` re-exports) and to `flux-runtime`'s
(`ExecutionEnvironment::with_resource_limits`, `Executor::with_resource_limits`,
`Executor::retained_result_bytes`). Nothing was removed or re-signatured.
