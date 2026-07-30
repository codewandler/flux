---
id: C-299
title: "A configured resource ceiling reaches neither sub-agents nor the `flux` binary"
pillar: Core
status: ready
priority: 5
areas: [flux-cli, flux-orchestrate, flux-runtime]
note: "C-290 built the ceiling and could reach neither consumer — flux-cli and flux-orchestrate were both fenced. Until this lands, `[limits]` is inert for the binary and `task`-delegated work is unbounded, while the SDK doc says the ceiling binds"
---

# A configured resource ceiling reaches neither sub-agents nor the `flux` binary

## Goal

C-290 gave an embedding host a real concurrency ceiling and a real retained-result ceiling, enforced
in the funnel every in-process tool call traverses. Two consumers never got wired to it, both because
they were fenced off by a concurrent story rather than by a design decision. Until this lands the
feature is narrower than its own documentation says.

**1. Sub-agents run unbounded.** `LocalSpawner::spawn` builds the child with
`Executor::new_with_authorization(...)` (`crates/flux-orchestrate/src/lib.rs:395-401`), which defaults
to `ResourceLimits::new()` — `grep -rn "ResourceLimits\|resource_limits" crates/` returns **zero**
hits in flux-orchestrate. So `task`-delegated work runs unbounded **in the same process**, while
`ClientBuilder::resource_limits` documents that the ceiling "binds for this in-process client"
(`crates/flux-sdk/src/lib.rs:750-752`). A host that sets a ceiling and then delegates has the ceiling
silently not apply to the delegated half.

**2. The `flux` binary ignores `[limits]`.** The new `max_concurrent_tool_calls`,
`tool_call_queue_timeout_ms` and `max_retained_result_bytes` keys are consumed today only by an
embedder calling `ResourceLimits::from_config`. `flux-cli`'s executor assembly never reads them, so a
configured key does nothing for anyone running the shipped binary — C-290's implementor documented
this rather than working around its fence, which was right, but it is real debt.

## Acceptance

- [ ] A failing-first test: a runtime with `max_concurrent_tool_calls(N)` that delegates through
      `task` exceeds N in flight across parent and child. That is the defect; assert on observed
      occupancy, not on configuration.
- [ ] Sub-agent executors inherit the parent's ceiling. **Decide and state whether the ceiling is
      shared or per-child** — a shared semaphore bounds total process concurrency, a per-child copy
      bounds each agent separately and multiplies under fan-out. They are different guarantees and the
      SDK doc must say which one it now means.
- [ ] ⚠ **Check the deadlock boundary before sharing anything.** C-290's review proved the
      re-entrancy exemption is a Tokio task-local that does **not** survive `tokio::spawn` — it
      probed exactly this and the nested acquire was refused. Today that is harmless *because* the
      child executor is unbounded and never contends for the parent's semaphore. Sharing the ceiling
      removes the very thing that makes it safe. A parent holding a slot while awaiting a child that
      queues on the same semaphore is a deadlock, bounded only by the queue timeout. Prove your shape
      does not do this, with a test, not an argument.
- [ ] `flux-cli` reads `[limits]` and applies it at executor assembly, so a configured key binds for
      the shipped binary.
- [ ] `website/docs/reference/config.md:204-208` lists the new keys — it currently documents only
      `turn_token_budget` under "Resource limits", so an operator gets no signal that the others exist.
- [ ] Full gate green, including `FLUX_BWRAP_BIN=/nonexistent/bwrap`.

## Notes

- Both gaps were found by C-290's independent review, not by its implementor — and neither was
  avoidable in that story: flux-cli and flux-orchestrate were fenced for C-213 and C-277 respectively.
  This is the cost of a wide wave, paid deliberately.
- ⚠ Sequencing: the `flux-cli` half is one call site and cheap. The sub-agent half is a design
  decision with a deadlock hazard attached. They can ship separately, and if this story is split, ship
  the CLI wiring first — it is the one an operator can observe.
- Related: [C-290](C-290-runtime-resource-limits.md) built `ResourceLimits`;
  [C-298](C-298-evidence-log-is-the-dominant-unbounded-retention.md) is the other thing C-290 could
  not reach.
