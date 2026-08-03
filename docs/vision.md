# flux — vision & principles

This document states *why* flux exists and the principles that decide how it's built. It is the
tie-breaker when a design choice is unclear: prefer the option that best serves the north star and
the principles below.

## What flux is

A Rust **agent SDK, harness, and coding agent** built as one Cargo workspace of small,
strictly-layered crates. Its defining idea: **the LLM is not the runtime.** Models participate inside
typed stages: they interpret intent, gather evidence through exact native operation schemas, ask for
decisions, and propose literal actions. Authored Flux-Lang owns control flow, and a deterministic Rust
runtime freezes effects into batches and executes them through one mandatory chain
(**authorization → approval → guarded IO**). The model never authors executable Flux.

## Three pillars

flux is one platform on that thesis, with three **co-equal** pillars. The safety core, providers,
tools, and orchestration are shared machinery beneath them:

1. **The Agent** — a zero-config personal coding agent (CLI/TUI), an embeddable Rust SDK, and a
   deployable HTTP server. The pillar most users touch; its internal surface priority is set out in
   *Audience & priority* below.
2. **The Language (Flux-Lang)** — the authored workflow language for deterministic flows, reusable
   operations, adaptive agent loops, and durable journeys. It is readable, analyzer-validated, and
   deliberately smaller than a general-purpose language. Models are called *from* typed stages; they
   do not generate the program.
3. **The Improvement Loop** — the eval + self-improvement harness (`flux-eval`), kept inside the repo
   because it is used directly to make flux better at real coding work; the closer to the code, the
   better. *Status:* the loop's machinery is proven end-to-end, but the autonomous initiative is
   **on hold** (de-prioritized 2026-07-06) until a statistically clean headline gain is confirmed —
   this pillar is currently aspirational, and this document says so honestly. See
   [self-improvement/STATUS.md](self-improvement/STATUS.md).

## North star: the LLM is not the runtime

**The single property flux must get right above all else is that the model proposes and the runtime
disposes.** Mainstream agents let an LLM's transcript become the runtime contract. Flux gives the
model bounded semantic jobs while authored control flow and host-owned contracts decide what can
happen: signals narrow a live catalog, reads return evidence, effects become immutable action
batches, and execution remains under policy. Everything else flux is proud of falls out of that
boundary:

- **Determinism & repeatability** — authored flows own order, bounds, branches, suspension, and resume.
  Because a run is a deterministic artifact, flux delivers what no LLM-as-runtime framework can:
  **hermetic replay, fork-at-any-decision, and run-diff** (`flux replay` / `fork` / `diff`), and —
  for SDK embedders — **Test · Tune · Resurrect**: record a run once and re-run the real agent
  offline in `cargo test` for $0, re-run a recorded session under exactly one changed variable, and
  finish a turn killed mid-execution with zero model re-spend.
- **Token savings & speed** — a native stage keeps one valid provider ledger and repairs locally.
- **Auditability** — intent, evidence, proposed batches, receipts, and execution reports are explicit.
- **Safety by construction** — every plan node lowers onto one envelope. All IO goes through
  `flux-system`; all ops through `Executor::dispatch`. Default-deny authorization (grants over
  subjects × resources × actions, gated by trust + scopes, with a usable local default so the agent
  still works out of the box); destructive and policy-flagged effects forced to human approval even
  under permissive rules; secrets redacted from model-visible output and never off the machine.
  Defense-in-depth reaches below the envelope too: an opt-in OS sandbox (bubblewrap on Linux,
  Seatbelt on macOS) confines spawned processes at the single spawn choke point.

Safety is no longer billed as *the* headline — it is one of the guarantees the architecture buys. It
stays non-negotiable: the envelope is the one choke point that no tool, plugin, sub-agent, or surface
path may route around, a new bypass is a release blocker, and the no-bypass invariants are covered by
tests.

## Audience & priority (within the Agent pillar)

The Agent pillar ships in this order, and ambiguity is resolved in favor of the earlier tier:

1. **Personal coding agent** — a zero-config CLI/TUI that is a credible daily driver for real coding
   work on your own machine. This comes first; if a platform feature would compromise the local
   experience, the local experience wins.
2. **Reusable agent SDK** — a library others embed to build their own safe agents. The CLI is the
   reference application built on the same SDK; the SDK is not an afterthought.
3. **Multi-user platform** — a deployable server with per-user identity and policy. The substrate is
   real (an authenticated HTTP API, per-request principal auth with per-principal isolation, an
   opt-in Postgres backend for >1-replica deployments, A2A conformance); it continues to harden as
   the first two tiers solidify.

**Downstream consumers validate tiers 2–3 in practice.** Managed-agent services and Slack-channel assistants
build on `flux-sdk` by path-dependency, and drive two platform-tier surfaces flux now carries:
**event-trigger channels** (an agent *woken by* a schedule, webhook, or Slack mention — not only reached
request/response) and a **knowledge/datasource layer** (answers grounded in an indexed corpus).
Both sit **behind the same envelope — no new bypass** — and the personal-coding-agent-first priority above
is unchanged; these are platform-tier capabilities, hardened as the earlier tiers solidify.

## Integration boundary

Every official external integration is a connector, and Exchange is the only official integration
executor. flux-connectors owns the declaration, schema, effects, runtime plan and any
vendor-specific artifact. Exchange owns credentials, grants, installation, execution and lifecycle.
Flux embeds one native Exchange client and retains the model loop, authorization, approval and tool
projection; it owns no connector runtime host and has no plugin fallback.

That embedded client is accepted direction, not shipped behavior. Existing integration-specific
crates under `plugins/` remain a temporary compatibility path while each Exchange replacement proves
the legacy contract. C-506 then removes the protocol, host, installer, signed pack and release
artifacts unconditionally. Flux remains useful without Exchange for its language, agent loop, SDK
and core tools; official external integrations are unavailable when Exchange is unavailable.

## Principles

1. **The LLM is not the runtime** (the north star, above) governs everything: a model participates in
   typed stages but never authors executable Flux or drives IO directly. **Non-bypassable safety** is the
   hard invariant this buys — no tool, plugin, sub-agent, or surface path reaches real
   filesystem / process / network IO without traversing the one envelope, and a bypass is a release
   blocker.
2. **Strict layering.** Crates are stratified L0 (pure contracts) → L6 (surfaces); a crate may
   depend only on its own layer or lower. This is enforced by a test, not a convention. It keeps the
   safety core small, auditable, and impossible to route around from a surface.
3. **Provider-neutral, never locked in.** A provider is a *wire codec × credential* cell; adding one
   is a small composition. flux must never become Anthropic-only (or any single vendor). Multi-provider
   routing (`provider/model`) is first-class.
4. **Local-first & private.** No telemetry, no phone-home, no background data egress. Secrets stay on
   the box. What runs on your machine is yours.
5. **Zero-config, opt-in complexity.** `flux` with no arguments is a working agent. Power
   (policy grants, hooks, plugins, orchestration) is available but never required to start.
6. **Quality over quantity — never "vibecoded slop."** flux is the opposite of a sprawling,
   bug-ridden codebase with thousands of open issues. Correctness, a small well-understood surface,
   and a permanently green gate (tests + clippy `-D warnings` + fmt + the layering lint) outrank
   feature count. Every behavioral change ships with a test that fails before it. A feature that
   can't be held to the bar doesn't ship.
7. **Auditable & durable.** Sessions are event-sourced and resumable; tool calls, destructive
   markers, skill activations, and compaction are recorded as evidence. You can always explain what
   the agent did and why it was allowed.

## Non-goals

- **Provider lock-in.** No single-vendor coupling in the core.
- **Low-quality sprawl.** No merging of unreviewed, untested, or layering-violating code to chase
  breadth; no accumulation of an unmaintained issue backlog. Depth and correctness first.
- **Telemetry / hosted SaaS dependence.** flux is something you run, not something that runs you.

(An agent GUI/IDE *product* and a managed cloud offering are simply *out of current scope*, not
forbidden — the roadmap is CLI/TUI/SDK/HTTP. Flux-Lang *editor tooling* is in scope and shipped:
the `flux-lsp` language server plus the tree-sitter/TextMate/IntelliJ grammars. Revisit the rest
only with a concrete need.)

## Openness

Public open-source, dual-licensed **MIT OR Apache-2.0**, published to crates.io as
`codewandler-flux-*`, contributions welcome. Because the quality
bar is a principle (not a nicety), contributions are held to it: the green gate and the no-bypass
safety tests are the price of entry. See [AGENTS.md](../AGENTS.md) for the contributor contract.

## How success is measured

- Every effect is traceable from intent and evidence through an approved batch to guarded execution.
- A reviewer can trace *every* IO path to the envelope and find no bypass.
- `flux` is a tool the author reaches for by default for real coding tasks.
- A third party can build a safe agent on `flux-sdk` without touching the core.
- The gate is green on every commit, and the issue list reflects deliberate, scoped work — not
  accumulated debt.

---

See [architecture.md](architecture.md) for the design that implements this, and [roadmap.md](roadmap.md)
for status and what's next.
