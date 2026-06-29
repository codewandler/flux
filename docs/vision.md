# flux — vision & principles

This document states *why* flux exists and the principles that decide how it's built. It is the
tie-breaker when a design choice is unclear: prefer the option that best serves the north star and
the principles below.

## What flux is

A Rust **agent SDK, harness, and coding agent** built as one Cargo workspace of small,
strictly-layered crates. Its defining idea: **the LLM is not the runtime.** Instead of letting the
model drive execution tool-by-tool, flux uses it as a **compiler front-end** — the model turns a
request into a typed, readable execution **plan** (a Flux-Lang graph), and a deterministic Rust
runtime executes that plan through one mandatory chain (**authorization → approval → guarded IO**).
You see the plan before it runs, and the same plan can be re-run.

## Three pillars

flux is one platform on that thesis, with three **co-equal** pillars. The safety core, providers,
tools, and orchestration are shared machinery beneath them:

1. **The Agent** — a zero-config personal coding agent (CLI/TUI), an embeddable Rust SDK, and a
   deployable HTTP server. The pillar most users touch; its internal surface priority is set out in
   *Audience & priority* below.
2. **The Language (Flux-Lang)** — the typed plan format the agent compiles into. It is
   **machine-generated** (emitted from natural language as JSON or native text), **human-readable**
   (you audit every plan before it runs), and **lightly human-editable** (nudge a plan, not author
   one from scratch). It is deliberately *not* a hand-written general-purpose language.
3. **The Improvement Loop** — the eval + self-improvement harness (`flux-eval`), kept inside the repo
   because it is used directly to make flux better at real coding work; the closer to the code, the
   better.

## North star: the LLM is not the runtime

**The single property flux must get right above all else is that the model proposes and the runtime
disposes.** Mainstream agents make the LLM the *runtime scheduler* — it picks each step live, and the
whole transcript is re-sent every turn so it can choose the next move. That is slow, expensive,
non-deterministic, and injectable. flux inverts it: the model is a compiler front-end that emits a
typed execution graph; a deterministic Rust runtime resolves symbols to stored values and executes
registered operations under policy. Everything else flux is proud of **falls out of that inversion**:

- **Determinism & repeatability** — a plan is an artifact; re-running it costs the fewest model calls.
- **Token savings & speed** — raw tool outputs are stored as symbols, not re-sent every turn.
- **Auditability** — a turn *is* a graph you read like Go or Rust before it runs, not a black box.
- **Safety by construction** — every plan node lowers onto one envelope. All IO goes through
  `flux-system`; all ops through `Executor::dispatch`. Default-deny authorization (grants over
  subjects × resources × actions, gated by trust + scopes, with a usable local default so the agent
  still works out of the box); destructive and policy-flagged effects forced to human approval even
  under permissive rules; secrets redacted from model-visible output and never off the machine.

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
3. **Multi-user platform** — a deployable server with per-user identity and policy. The seams exist
   (HTTP API, OIDC identity); they are hardened as the first two tiers solidify.

**Downstream consumers validate tiers 2–3 in practice.** The managed-agents managed-agents service and the
downstream Slack-channel assistant both build on `flux-sdk` by path-dependency, and drive two platform-tier surfaces flux
now carries: **event-trigger channels** (an agent *woken by* a schedule, webhook, or Slack mention — not
only reached request/response) and a **knowledge/datasource layer** (answers grounded in an indexed corpus).
Both sit **behind the same envelope — no new bypass** — and the personal-coding-agent-first priority above
is unchanged; these are platform-tier capabilities, hardened as the earlier tiers solidify.

## Principles

1. **The LLM is not the runtime** (the north star, above) governs everything: a turn compiles to a
   plan the runtime executes; the model never drives IO directly. **Non-bypassable safety** is the
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

(GUI/IDE surfaces and a managed cloud offering are simply *out of current scope*, not forbidden —
the roadmap is CLI/TUI/SDK/HTTP. Revisit only with a concrete need.)

## Openness

Public open-source, dual-licensed **MIT OR Apache-2.0**, contributions welcome. Because the quality
bar is a principle (not a nicety), contributions are held to it: the green gate and the no-bypass
safety tests are the price of entry. See [AGENTS.md](../AGENTS.md) for the contributor contract.

## How success is measured

- Every turn is an auditable plan a user can read before it runs, and the same plan re-runs deterministically.
- A reviewer can trace *every* IO path to the envelope and find no bypass.
- `flux` is a tool the author reaches for by default for real coding tasks.
- A third party can build a safe agent on `flux-sdk` without touching the core.
- The gate is green on every commit, and the issue list reflects deliberate, scoped work — not
  accumulated debt.

---

See [architecture.md](architecture.md) for the design that implements this, and [roadmap.md](roadmap.md)
for status and what's next.
