# flux — vision & principles

This document states *why* flux exists and the principles that decide how it's built. It is the
tie-breaker when a design choice is unclear: prefer the option that best serves the north star and
the principles below.

## What flux is

A Rust **agent SDK, harness, and coding agent** built as one Cargo workspace of small,
strictly-layered crates. Every tool call — built-in, plugin, or sub-agent — passes through a single
mandatory chain (**authorization → approval → guarded IO**), so the agent can edit code and run
commands without being able to escape the workspace or leak secrets.

## North star: safe by construction

**The single property flux must get right above all else is non-bypassable safety.** There must be
no tool, plugin, sub-agent, or surface path that reaches real filesystem / process / network IO
without traversing the one envelope. Safety is the differentiator, not a feature — it is what makes a
capable autonomous agent trustworthy enough to actually run.

Concretely, "safe by construction" means:
- One choke point. All IO goes through `flux-system`; all tools go through `Executor::dispatch`.
- Default-deny authorization (grants over subjects × resources × actions, gated by trust + scopes),
  with a usable local default so the agent still works out of the box.
- Destructive operations and policy-flagged effects are forced to human approval even under
  permissive rules.
- Secrets are redacted from model-visible output and never leave the machine.
- A new bypass is a release blocker, and the no-bypass invariants are covered by tests.

## Audience & priority

flux is built in this order, and ambiguity is resolved in favor of the earlier tier:

1. **Personal coding agent** — a zero-config CLI/TUI that is a credible daily driver for real coding
   work on your own machine. This comes first; if a platform feature would compromise the local
   experience, the local experience wins.
2. **Reusable agent SDK** — a library others embed to build their own safe agents. The CLI is the
   reference application built on the same SDK; the SDK is not an afterthought.
3. **Multi-user platform** — a deployable server with per-user identity and policy. The seams exist
   (HTTP API, OIDC identity); they are hardened as the first two tiers solidify.

## Principles

1. **Non-bypassable safety** (the north star, above) governs everything.
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

- A reviewer can trace *every* IO path to the envelope and find no bypass.
- `flux` is a tool the author reaches for by default for real coding tasks.
- A third party can build a safe agent on `flux-sdk` without touching the core.
- The gate is green on every commit, and the issue list reflects deliberate, scoped work — not
  accumulated debt.

---

See [architecture.md](architecture.md) for the design that implements this, and [roadmap.md](roadmap.md)
for status and what's next.
