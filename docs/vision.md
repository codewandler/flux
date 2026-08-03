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

## Contributor decision register: what to close and what to preserve

This is an internal decision register, not product positioning or a scorecard. It closes the loop on
the atomic findings in the
[2026-08-01 comparative source review](reviews/single/2026-08-01-pi-flux-harness-comparison.md), so a
contributor does not optimize a comparative number without deciding what the change would buy and
cost. Each finding ID below appears once, in exactly one bucket. Combined review findings such as F4
and F5 are split where they make independent claims; the suffixes make that split explicit. The
review's already-closed July-baseline items are one historical entry rather than new work.

The review's bottom line remains the tie-breaker: *"choose Flux when the runtime must remain the
authority after the model, prompt and workflow have spoken."* This register does not defend every
shortcoming. In particular, secure SDK defaults, interactive confinement, Windows isolation and
cancellation coverage are real gaps, not the price of having an envelope.

### Close it

These findings weaken Flux on its own terms. Closing one still requires evidence that the repair does
not create a bypass.

| Finding | Decision and owner |
| --- | --- |
| **C1 — surface-dependent confinement (F2)** | Close the autonomous SDK-default gap in [C-444](stories/C-444-sdk-secure-defaults.md), re-take the interactive and plugin-startup decision in [C-445](stories/C-445-interactive-confinement-posture.md), and either provide or plainly disclaim Windows OS isolation in [C-446](stories/C-446-no-windows-sandbox-backend.md). These are real gaps: policy/guarded IO is mandatory, but OS confinement is not yet available or selected on every surface. |
| **C2 — serialized turns (F3)** | Measure the throughput ceiling and prove what the mutex protects before deciding whether it stays in [C-447](stories/C-447-the-per-engine-turn-mutex.md). Serialization buys simple identity, ordering and provider-history validity, but costs shared-engine concurrency; until the investigation lands, neither deleting nor defending the mutex is justified. |
| **C3 — unbounded SDK and delegated-tree defaults (F4b)** | [C-444](stories/C-444-sdk-secure-defaults.md) owns the reviewed default: an autonomous embedder should not silently get no ceiling. This is distinct from deliberately process-local server governance below. |
| **C4 — cancellation coverage not demonstrated** | Audit model calls, retries, compaction, spawned processes, sub-agents and journeys in [C-448](stories/C-448-cancellation-coverage.md). The review credited Flux's invariants, but another harness demonstrated broader reach; an effect continuing after cancellation is a real gap if found. |
| **C5 — provider catalogue and first-use reachability** | Decide the maintenance model and document the compatible/extension route in [C-449](stories/C-449-provider-breadth.md). A missing named provider is an adoption gap even though it is not a codec-quality defect. |
| **C6 — direct-dependency review discipline** | Transfer the useful mechanical pinning and build-script review idea in [C-450](stories/C-450-dependency-pinning-discipline.md). Flux's overall assurance was stronger, but that narrow supply-chain control was genuinely behind. |
| **C7 — performance and task-quality claims are unmeasured** | Run the same-model, same-task comparison in [C-451](stories/C-451-the-head-to-head-benchmark.md). Source reading does not establish success rate, latency, token cost, memory or throughput. |
| **C8 — embedder and operator friction** | Keep improving the documented SDK, server and terminal paths through the ordinary roadmap and [documentation gap audit](stories/C-442-peer-docs-gap-audit.md). Richer safety controls may cost concepts, but avoidable setup and missing explanations are not defended complexity. |
| **C9 — July assurance residuals** | The review records server controls, adversarial CI, pinned Actions and release provenance as closed or substantially closed. Classification metadata remains part of the trusted computing base and continues under the existing registry/codegate checks; these are completed remediation, not trade-offs to reverse. |

### Defend it

These choices remain deliberate. Each defence states both its purchase and its bill.

| Finding | What it buys | What it costs |
| --- | --- | --- |
| **D1 — mandatory authored control flow and effect envelope (P1; loop/defaults ratings)** | The runtime remains authoritative: model proposals cannot bypass authorization, approval and guarded IO, and authored bounds/history rules remain enforceable. | More types, policy concepts, dispatch work and latency than a direct host-authority loop; it is not the smallest or most permissive embedding substrate. |
| **D2 — scoped extensions rather than maximum in-process replacement (P2, F1; extensions rating)** | Cooperative plugin callbacks and sub-agents can be authority-narrowed and sent through dispatch instead of replacing the safety core. | Less customization freedom, and installed native plugin binaries are still trusted host code—not a hostile-plugin sandbox. Truly untrusted extensions still require a container, VM or separate hardened worker. |
| **D3 — first-class budgets and bounded delegation (P3)** | Unattended agents have runtime-use and delegation controls in the harness rather than depending only on an outer watchdog. | More configuration and accounting machinery; limits still require an operator to choose an appropriate posture and do not become a cluster quota plane. |
| **D4 — strict layered workspace and mandatory envelope (F5a; performance/complexity rating)** | Small layer-owned contracts, mechanical dependency direction and one guarded effect path keep the no-bypass claim auditable. | More crates, slower onboarding, higher audit/change-coordination cost and likely runtime overhead. Removing layers or the envelope merely to improve a complexity score would destroy the property Flux exists to provide. Measure the cost under C7 instead. |
| **D5 — process-local resource governor (F4a)** | Limits stay deterministic at the process boundary and the core does not pretend to coordinate replicas it cannot observe. | A multi-replica operator must supply a reverse proxy or shared rate/spend plane; one Flux process is not a distributed quota authority. |
| **D6 — personal-agent-first product priority (operator-UX rating)** | The CLI/TUI remains a coherent daily-driver rather than every platform feature taking precedence. | Some server, RPC and deep-customization work matures later. This priority does not excuse avoidable UX or documentation gaps (C8). |

### Not code

These are evidence and adoption conditions. Code can make Flux worthy of adoption, but cannot declare
them closed.

| Finding | Why it is not an implementation closure |
| --- | --- |
| **N1 — public ecosystem, bus factor and integration discovery (F5b; ecosystem rating)** | Stars, forks, contributors and an established extension catalogue accrue through real use. A larger ecosystem lowers discovery and maintenance risk; it does not prove execution safety. |
| **N2 — independent production history and pre-1.0 maturity (F5c)** | Operating evidence, external audits and incident history must come from independent deployments over time. Tests and internal review are necessary evidence, not substitutes for that history. |
| **N3 — the comparison project's experimental server contract (P4)** | That finding describes the other reviewed tree. It informs deployment comparison but is not Flux work. |
| **N4 — the comparison snapshot itself** | Scores, repository counts and release versions were observations at pinned commits, not product requirements. Re-run the review after remediation; do not encode a target score. |

Honesty here follows the same standard as the pillars above: the Improvement Loop remains **currently
aspirational**, and this document says so. A future register update must preserve that distinction
between shipped guarantees, chosen costs, open engineering gaps and evidence that only users and time
can supply.

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
