---
id: C-444
title: "`auto_approve(true)` does not imply confinement, and SDK resource ceilings are unbounded by default"
pillar: Core
status: in-progress
priority: 3
design: docs/designs/pi-comparison-remediation.md
epic: pi-comparison-remediation
areas: [flux-sdk]
note: "⚠ the finding that undercuts flux's headline claim. Both are DOCUMENTED, and documented is not defaulted — an embedder reading the headline and not the caveat gets auto-approval with no confinement and no ceiling, which the review itself calls a poor fit"
---

# Documented is not defaulted

## Goal

Make it impossible for an embedder to reach auto-approval with no OS confinement and no resource
ceiling by following the documented happy path.

## The two findings

From the 2026-08-01 Pi comparison, both line-cited:

- **F2** — *"The SDK also states that `auto_approve(true)` does not imply confinement; the embedder must
  set it"* (`crates/flux-sdk/src/lib.rs:17`).
- **F4** — *"The SDK's runtime-use ceilings are unbounded by default and per agent; a delegated tree can
  multiply its concurrent tool count"* (`crates/flux-sdk/src/lib.rs:792`).

⚠ flux's argument is that authorization and approval are **runtime types that cannot be disabled**.
These two are where an embedder falls out of that without noticing — and it is why the review scores
Embeddability 8.0 against Pi's 9.0 with the reading *"asks more of the embedder."* That phrase is
usually about ergonomics; here it is about safety.

## Acceptance

- [x] **Failing-first**: a test constructing an SDK agent the documented way with `auto_approve(true)`
      and asserting it is confined and has a resource ceiling — failing at the merge base.
      → `crates/flux-sdk/tests/secure_defaults.rs`; at `5ba0a91f`, 2 of 4 failed (`Off` vs `Require`).
- [x] ⚠ **Decide and record: does `auto_approve(true)` imply confinement, or refuse without an explicit
      confinement decision?** Both are defensible; silently doing neither is not. The CLI's precedent is
      C-410 — unattended surfaces fail *closed* — and an SDK embedder is the same posture with no human
      at a terminal.
      → **Implies confinement**, following C-410 exactly: `Envelope::resolve_sandbox` raises an
      autonomous posture to `SandboxMode::Require` with the sandbox network closed
      (`crates/flux-sdk/src/envelope.rs:105`). Refusing was rejected because it makes a valid posture
      cost an extra required call, which reads as "autonomy is discouraged" (C-463 says it is not).
      The raise is a floor over *silence* only: `with_sandbox` wins outright, a stricter ambient
      `FLUX_SANDBOX` still applies, and an injected `Approver` triggers nothing.
- [x] A default resource ceiling exists, and ⚠ **a delegated tree cannot multiply past it** — per-agent
      ceilings that compose into an unbounded total are the actual finding, not the per-agent number.
      → `ResourceLimits::autonomous()` (`crates/flux-runtime/src/limits.rs`) sets both a per-agent
      concurrency ceiling (16) and `max_live_agents` (8), so the tree total is 128 rather than
      unbounded. The census is an `AgentCensus` **shared** across `independent_copy` — sound precisely
      because it *refuses* rather than queues, so it cannot enter the wait cycle that makes a shared
      semaphore deadlock (C-299). `LocalSpawner::spawn` takes a place and holds it for the child's whole
      turn. The per-agent semaphore is unchanged.
- [ ] ⚠ **This is a breaking change for embedders and owes a MINOR** under the pre-1.0 rule. Existing
      embedders get behaviour they did not ask for; that is the point, but it must be deliberate, in the
      CHANGELOG, and in WHATS-NEW with an action line.
      → **Not done: `CHANGELOG.md` and `WHATS-NEW.md` are fenced for this implementor.** Handed to the
      coordinator. The change is breaking as predicted and the migration line is: an embedder using
      `auto_approve(true)` on a host with no sandbox backend now fails closed at the first spawn, and
      declines the raise with `.with_sandbox(Sandbox::resolve(SandboxSettings::off()))`.
- [x] The SDK docs stop *warning* about the gap and describe the new default. A caveat that is no longer
      true is worse than one that is.
      → Rewritten: the `flux-sdk` crate root, the `Sandbox` and `ResourceLimits` re-export docs,
      `ClientBuilder::{auto_approve, with_sandbox, resource_limits}`, the three `FlowClientBuilder`
      mirrors, `SubAgents::with_resource_limits`, `flux_config::Limits::max_concurrent_tool_calls`, and
      `website/docs/security/os-sandbox.md`. None of them present autonomy as degraded.
- [x] Full gate green — including `FLUX_BWRAP_BIN=/nonexistent/bwrap cargo test --workspace`, which
      caught the one real regression (see Progress).

## Notes

- ⚠ Highest priority in this epic: it is the only finding where flux's own headline claim is weaker than
  it reads.
- The escape hatch matters as much as the default — an embedder who genuinely wants no confinement must
  be able to say so explicitly, and that call should be visible in their code.
- Related: C-410 raised the unattended CLI surfaces to fail-closed; this is the same argument one layer
  out.

## Progress
- Filed 2026-08-02 from the Pi comparison.
- 2026-08-02 — implemented on `impl/C-444` (merge base `5ba0a91f`). The shape: the bug was that
  approval, confinement and ceilings were three independent knobs, so the fix couples them at
  *resolution* rather than making `auto_approve(true)` harder to call. `Envelope::resource_limits`
  became `Option<_>` so silence is distinguishable from a stated ceiling, and `resolve_sandbox` /
  `resolve_resource_limits` decide against the posture. `is_autonomous()` is deliberately narrow:
  blanket `auto_approve` **and** no injected approver, because a hand-written `Approver` is a policy
  this crate cannot read.
- The half worth re-reading before changing anything: **the tree ceiling is a census, not a semaphore.**
  C-299 established that sharing the execution semaphore across the `task` boundary deadlocks, and
  that reasoning still holds — so `max_live_agents` bounds the *other* factor in `N × k`. It is safe to
  share because it refuses instantly instead of queueing. Delete `with_max_live_agents` and
  `the_autonomous_preset_bounds_the_tree_as_well_as_the_agent` fails.
- ⚠ One real regression, found only by the no-backend gate run and *not* by `cargo test --workspace`:
  `crates/flux-app/tests/strict_review_journey.rs` builds an `auto_approve(true)` `FlowClient` and now
  resolved to `require`, which fails closed on any host without `bwrap` — i.e. every CI runner. Fixed by
  having that test state `SandboxSettings::off()` explicitly, which is also the correct posture for it:
  it compares an SDK path against an `App` path that does not go through the SDK envelope at all, so the
  raise would have compared two different postures. Any embedder test in this shape needs the same line.
- Left for the coordinator: the MINOR bump, the CHANGELOG entry and the WHATS-NEW action line (all
  fenced here). Adjacent, not fixed: `FlowClient` exposes no public `system()`/`resource_limits()`
  accessors, so `secure_defaults.rs` can only assert the raise on the `Client` door — the shared
  `Envelope` is what makes the two doors agree, but nothing test-visible proves it on the flow door.
