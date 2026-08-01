---
title: Pi and Flux agent harnesses — independent comparative source review
date: 2026-08-01
kind: internal-review
lens: security-and-production-readiness
method: >-
  two isolated source-level desk reviews using one shared nine-axis rubric, followed by primary
  evidence cross-check and synthesis; no task-quality benchmark, runtime benchmark, fuzzing,
  exploitation, paid-provider calls, or deployment exercise
reviewer: agent (two isolated harness reviewers; primary synthesis)
triage:
  kind: single
  status: open
  owner_stories: []
  aggregated_into: null
subject:
  repo: codewandler/flux
  version_in_tree: 0.48.0 (6ed0a0f7e6316f21dae929993959b323c67530f9)
  published_release_at_review: v0.48.0
  workspace_crates: 38
  comparison_repo: earendil-works/pi
  comparison_version_in_tree: 0.83.0 (aa0ec808b970db31822e07835a46647cb51d9d66)
  comparison_published_release_at_review: v0.83.0
overall_rating: 7/10
verdict: Pi is the better trusted-user coding substrate; Flux is the better controlled-effects substrate
ratings:
  security_architecture: 9/10
  secure_defaults: 6/10
  implementation_quality: 8.5/10
  security_assurance: 9/10
  release_supply_chain: 9/10
  product_maturity: 6.5/10
  community_bus_factor: 2/10
  production_readiness: 7/10
verification:
  status: verified against both pinned trees on 2026-08-01
  outcome: strongest conclusions cross-checked against code, tests, CI, release metadata, and official repository state
  material_errors: none found in either isolated review; comparative scores remain judgment, not measurement
top_findings:
  - Pi's default tools and extensions execute with host authority; it has no mandatory policy or isolation envelope
  - Flux centralizes effects behind authorization, approval and guarded IO, but interactive and SDK sandboxing remain opt-in
  - Both treat installed native extension code as trusted; Flux narrows cooperative plugin callbacks but cannot contain a hostile binary
  - Flux has materially closed its July assurance gaps, while per-engine serialization and process-local limits constrain scale-out
  - Pi leads in provider breadth, interactive customization and ecosystem; Flux leads in auditable control, resource budgets and unattended posture
---

## Verdict

There is no honest context-free winner.

**Pi is the stronger trusted-user coding harness.** Its loop is direct, its provider and session
support is broad, its SDK and RPC surfaces are easy to embed, and its in-process extension model is
exceptionally flexible. That flexibility is also its security boundary: stock Pi executes model
tools and extensions with the authority of the host process.

**Flux is the stronger controlled-effects harness.** Its model proposes typed calls while authored
Flux-Lang owns control flow, and each operation is re-decided through capability scope,
authorization, permission, approval, resource, guarded-IO and redaction gates. That makes Flux the
better foundation for unattended, multi-principal or exposed automation. The price is a much larger
conceptual/runtime surface, weaker ecosystem evidence, and lower throughput per shared engine.

For valuable unattended work, Flux is the safer starting point. For a developer sitting at a trusted
workstation and optimizing for speed, model choice and customization, Pi is the more mature product.
Neither should execute untrusted third-party native extensions without an outer container or VM.

The `overall_rating` and eight security ratings in frontmatter apply to **Flux**, preserving this
review series' stable axes. The pairwise ratings below use the broader harness rubric requested for
this comparison. They index the evidence; they are not a benchmark score.

## Scope and fairness rules

The Pi reviewer inspected only `earendil-works/pi` at
`aa0ec808b970db31822e07835a46647cb51d9d66`. The Flux reviewer inspected only this repository at
`6ed0a0f7e6316f21dae929993959b323c67530f9`. Both received the same rubric and were told that prose
is a claim, while code, tests, CI and release workflows are evidence. The synthesis then re-opened
the load-bearing evidence from both reports.

This review assesses the **harnesses**, not model intelligence. No same-model task corpus was run, so
it makes no claim about coding success rate, latency, token efficiency or answer quality. It also
does not confuse GitHub popularity with implementation quality.

The aspects that matter for this pairing are:

1. agent-loop and control-flow architecture;
2. tool/effect authorization, approval, isolation and shipped defaults;
3. extensions, plugins, skills and sub-agents;
4. providers, context, sessions and cancellation;
5. embeddability and automation surfaces;
6. operator experience and customization;
7. reliability, tests, CI, observability and releases;
8. performance and complexity footprint;
9. ecosystem, maintenance and production-fit boundaries.

These axes expose the pairing's real tradeoff. A raw feature inventory would reward Pi for deliberate
freedom and penalize Flux for deliberate controls without asking what happens when the user leaves
the keyboard.

## Ratings at a glance

| Axis | Pi | Flux | Evidence-backed reading |
| --- | ---: | ---: | --- |
| Agent loop / control flow | 8.5 | 9.0 | Pi is smaller and more direct; Flux makes the outer loop authored and enforces explicit iteration/history invariants. |
| Authorization / approval / isolation / defaults | 3.0 | 8.0 | This is the decisive split: trusted-process hooks versus a mandatory runtime envelope. Flux loses points because interactive/SDK OS confinement is still opt-in. |
| Extensions / plugins / skills / sub-agents | 8.0 | 8.0 | Pi's trusted in-process extension API is broader and easier; Flux adds first-class authority-narrowed sub-agents and scoped plugin callbacks. Neither contains a hostile native extension. |
| Providers / context / sessions / cancellation | 9.0 | 9.0 | Both are strong. Pi has exceptional provider breadth and session UX; Flux has resilient codecs and unusually explicit history/cancellation invariants. |
| Embeddability / automation | 9.0 | 8.0 | Pi offers agent core, SDK, JSON/RPC and protocol packages. Flux offers SDK, authored flows, server/A2A and replay/evidence, but asks more of the embedder. |
| Operator UX / customization | 9.0 | 8.0 | Pi is the more polished, freely customizable terminal product. Flux exposes richer safety and workflow controls at higher conceptual cost. |
| Reliability / CI / observability / releases | 8.0 | 9.0 | Both have serious supply-chain discipline. Flux adds architecture gates, no-backend tests, CodeQL, targeted Miri and artifact attestations. |
| Performance / complexity | 7.5 | 6.5 | Pi's core is direct and parallelizes tool work. Flux carries 38 crates and serializes turns per engine. No runtime benchmark was performed. |
| Ecosystem / maintenance / production fit | 8.0 | 7.0 | Pi has much stronger adoption and extension evidence; Flux has the stronger unattended safety substrate but far less external operating evidence. |

## Pi, reviewed on its own terms

### Strengths

Pi's loop is compact and legible. Application messages are transformed only at the model boundary,
and tool arguments are schema-validated before the optional interception hook and execution
([`packages/agent/src/agent-loop.ts:277-312`][pi-loop-boundary],
[`packages/agent/src/agent-loop.ts:600-663`][pi-tool-prepare]). Tool batches can execute concurrently
while preserving result ordering in the session
([`packages/agent/src/agent-loop.ts:489-553`][pi-tool-concurrency]).

Provider breadth is a major product advantage: the registry constructs 39 provider variants in the
pinned tree ([`packages/ai/src/providers/all.ts:86-127`][pi-providers]). Sessions are versioned trees
with parent links, branching, summaries, compaction and extension-persisted state
([`packages/coding-agent/src/core/session-manager.ts:30-80`][pi-sessions]). Cancellation reaches the
high-level agent and session-owned work, including retries, compaction and bash
([`packages/agent/src/agent.ts:306-323`][pi-agent-cancel],
[`packages/coding-agent/src/core/agent-session.ts:833-854`][pi-session-cancel]).

The embedding and customization story is unusually complete. The coding SDK accepts injected model,
resource, session and tool components ([`packages/coding-agent/src/core/sdk.ts:38-85`][pi-sdk]); the
product also exposes print/JSON/RPC modes and separate protocol/client/server packages. Extensions
can add or replace tools, commands, shortcuts, providers, compaction and UI in ordinary TypeScript.
This is a genuine productivity strength when the extension author and host are trusted.

Pi's engineering discipline is also stronger than its intentionally permissive runtime might
suggest. CI installs with lifecycle scripts disabled and runs checks/tests
([`.github/workflows/ci.yml:13-42`][pi-ci]); a scheduled workflow performs vulnerability and npm
registry-signature audits ([`.github/workflows/npm-audit.yml:1-31`][pi-audit]); direct dependencies
are mechanically pinned, and new dependency lifecycle scripts must be explicitly reviewed
([`scripts/check-pinned-deps.mjs:40-62`][pi-pins],
[`scripts/generate-coding-agent-shrinkwrap.mjs:224-253`][pi-lifecycle]). Third-party Actions are
commit-pinned in the reviewed workflows.

### Limitations and production boundary

**P1 — Pi has no mandatory authorization, approval or isolation envelope.** The project states this
plainly: it runs with the permissions of its host process ([`README.md:37-45`][pi-permissions]). The
default coding SDK enables read, bash, edit and write
([`packages/coding-agent/src/core/sdk.ts:245-251`][pi-default-tools]); filesystem tools accept absolute
and home-expanded paths rather than enforcing a workspace root
([`packages/coding-agent/src/core/tools/path-utils.ts:44-50`][pi-paths]); bash passes a model-supplied
command string to a shell and inherits the complete process environment
([`packages/coding-agent/src/core/tools/bash.ts:82-103`][pi-bash],
[`packages/coding-agent/src/utils/shell.ts:122-133`][pi-env]). On a secret-rich workstation, a
model-issued command can therefore reach credentials present in environment variables.

The low-level `beforeToolCall` hook can block a validated call
([`packages/agent/src/agent-loop.ts:600-656`][pi-tool-prepare]), so a consumer can build policy. It is
not a non-optional stock policy boundary: the coding agent wires it to extension interception
([`packages/coding-agent/src/core/agent-session.ts:460-488`][pi-before-tool]). Project trust is useful
but solves a different problem—it decides whether repository-local settings, skills and extensions
may load, not whether a model-issued effect is authorized
([`packages/coding-agent/src/core/project-trust.ts:24-95`][pi-project-trust]).

**P2 — Pi extensions are trusted arbitrary code.** The loader imports TypeScript/JavaScript and
executes the extension factory in-process
([`packages/coding-agent/src/core/extensions/loader.ts:412-490`][pi-extension-load]). Extensions can
replace built-ins, execute commands and register providers; Pi correctly documents that packages
have full system access ([`packages/coding-agent/README.md:404-408`][pi-package-trust]). This is an
excellent extension API, not a hostile-plugin boundary.

**P3 — stock high-level run budgets are weak.** The inner/outer loop continues while the model emits
tool calls or queues contain messages; a low-level `shouldStopAfterTurn` callback exists, but no
first-class maximum-turn, tool-call, wall-time or spend limit was found on the high-level `Agent`
options ([`packages/agent/src/agent-loop.ts:163-274`][pi-loop],
[`packages/agent/src/types.ts:207-217`][pi-stop-hook],
[`packages/agent/src/agent.ts:96-121`][pi-agent-options]). An unattended Pi deployment needs an outer
watchdog even when it is containerized.

**P4 — the remote server is not yet a stable production contract.** Its package README marks APIs
and behaviour experimental and records an incomplete supervision migration
([`packages/server/README.md:1-4`][pi-server-experimental],
[`packages/server/README.md:43-57`][pi-server-migration]). This is not absence of security work: the
server requires a non-empty token, uses hashed constant-time authentication, bounds frames and
handshake time, and gives Unix sockets restrictive permissions
([`packages/server/src/server.ts:30-60`][pi-server-config],
[`packages/server/src/server.ts:222-259`][pi-server-auth],
[`packages/server/src/transports/unix/listener.ts:225-240`][pi-unix]).

### Pi deployment recommendation

- Strong fit: a trusted developer workstation, provider experimentation, a deeply customized
  terminal agent, or embedding in a Node/Bun application.
- Conditional fit: CI/unattended work inside an ephemeral container or VM with scrubbed environment,
  read-only host mounts, explicit network policy, reviewed extensions and external wall/cost limits.
- Poor stock fit: adversarial repositories, multi-tenant service, regulated execution, a secret-rich
  host, or any workload whose policy must be structurally non-bypassable.

## Flux, reviewed on its own terms

### Strengths

Flux's central differentiator is present in code. `ExecutionAuthorization` has no disabled profile
([`crates/flux-runtime/src/lib.rs:2440`](../../../crates/flux-runtime/src/lib.rs#L2440)), and every live
dispatch checks disabled operations, active capability scope, physical filesystem identity,
authority requirements, default-deny policy and deny-first permissions before reaching approval and
execution ([`crates/flux-runtime/src/lib.rs:3777`](../../../crates/flux-runtime/src/lib.rs#L3777)).
The generic bash tool is high-risk, approval-gated and excluded from the advertised tool set unless
the shell group is explicitly enabled
([`crates/flux-tools/src/lib.rs:1275`](../../../crates/flux-tools/src/lib.rs#L1275)).

The agent loop is an authored Flux-Lang program, not executable control flow invented by the model
([`crates/flux-flow/assets/agent-loop.flux:1`](../../../crates/flux-flow/assets/agent-loop.flux#L1)).
`AgentSpec` makes tools, permissions, loop, model budgets and compaction explicit, with a default
maximum of 50 authored iterations
([`crates/flux-agent/src/lib.rs:163`](../../../crates/flux-agent/src/lib.rs#L163)). Cancellation and
termination paths are designed around valid provider history, and the same `FlowEngine` backs CLI,
SDK and served turns.

Flux also has the stronger constrained-delegation model. Sub-agents inherit authorization and
intersect their role tools with the active capability scope; plugin operations traverse the same
dispatcher, while cooperative host callbacks are manifest- and operator-scoped. The combination is
substantially safer than arbitrary in-process extensions even though native plugin code remains
trusted.

Assurance has moved materially since the July baseline. CI now tests the normal and no-sandbox-
backend postures, dependency scanning covers the root and plugin workspaces, CodeQL and targeted
Miri are present, third-party Actions are SHA-pinned, and release artifacts receive provenance
attestations ([`.github/workflows/ci.yml:93`](../../../.github/workflows/ci.yml#L93),
[`security-audit.yml:32`](../../../.github/workflows/security-audit.yml#L32),
[`adversarial-assurance.yml:22`](../../../.github/workflows/adversarial-assurance.yml#L22),
[`release.yml:409`](../../../.github/workflows/release.yml#L409)). Server defaults now include body,
timeout, concurrency, call and spend controls
([`crates/flux-server/src/lib.rs:654`](../../../crates/flux-server/src/lib.rs#L654)).

### Limitations and production boundary

**F1 — native plugins remain trusted dependencies.** The SDK says this explicitly: a plugin binary
is host native code, not an OS-sandboxed extension
([`crates/flux-sdk/src/lib.rs:258`](../../../crates/flux-sdk/src/lib.rs#L258)). Manifest-scoped
callbacks reduce accidental/cooperative authority; they cannot stop a malicious binary from using
the OS APIs available to its process. Do not market Flux as an untrusted plugin sandbox.

**F2 — confinement defaults depend on surface.** The underlying sandbox remains off with open
network when nothing requests it
([`crates/flux-system/src/sandbox.rs:35`](../../../crates/flux-system/src/sandbox.rs#L35)). The CLI now
raises unattended, auto-approved and serving surfaces to fail-closed `require` and defaults their
sandbox network closed
([`crates/flux-cli/src/dispatch.rs:253`](../../../crates/flux-cli/src/dispatch.rs#L253)). Interactive
turns remain deliberately exempt—even installed plugin startup can run unconfined
([`crates/flux-cli/src/dispatch.rs:111`](../../../crates/flux-cli/src/dispatch.rs#L111)). The SDK also
states that `auto_approve(true)` does not imply confinement; the embedder must set it
([`crates/flux-sdk/src/lib.rs:17`](../../../crates/flux-sdk/src/lib.rs#L17)). Windows has no native
backend in the reviewed tree. Thus Flux has a mandatory policy/guarded-IO boundary everywhere, but
not a mandatory OS isolation boundary everywhere.

**F3 — one `FlowEngine` serializes turns.** All public turn entries acquire the same `turn_gate`
mutex ([`crates/flux-flow/src/engine.rs:713`](../../../crates/flux-flow/src/engine.rs#L713)). This is a
strong identity/session-integrity simplification and a real throughput ceiling for a server sharing
one engine. Scale-out requires multiple engines/replicas rather than treating one engine as a
high-concurrency scheduler.

**F4 — distributed resource governance belongs outside the process.** Server body, timeout, rate,
concurrency, call and spend controls exist, but the governor is intentionally process-local
([`crates/flux-server/src/resource.rs:1`](../../../crates/flux-server/src/resource.rs#L1)). The SDK's
runtime-use ceilings are unbounded by default and per agent; a delegated tree can multiply its
concurrent tool count
([`crates/flux-sdk/src/lib.rs:792`](../../../crates/flux-sdk/src/lib.rs#L792)). Multi-replica service
needs a reverse proxy/shared quota plane, and an embedder needs explicit `ResourceLimits`.

**F5 — complexity and ecosystem evidence lag the architecture.** The workspace has 38 crates and a
large policy, flow, provider, plugin, orchestration and surface area. That is not itself a defect,
but it raises audit, onboarding and change-coordination cost. The repository remains pre-1.0, and
the security policy supports only the latest `0.x` minor
([`SECURITY.md:35`](../../../SECURITY.md#L35)). At review time the official GitHub repository showed
negligible public adoption. Internal review density is high; independent maintainer depth and
production operating history are not established.

### Movement since the 2026-07-29 baseline

The earlier review's architecture-versus-assurance gap has narrowed substantially:

| Baseline complaint | Status at 0.48.0 | Evidence |
| --- | --- | --- |
| Unattended execution could run without an OS sandbox | **Closed for classified CLI surfaces; still open for interactive/SDK defaults** | `crates/flux-cli/src/dispatch.rs:253-395`; `crates/flux-sdk/src/lib.rs:17-25` |
| No server body/timeout/rate controls | **Closed in-process** | `crates/flux-server/src/lib.rs:654-729`; distributed governance remains external |
| No advisory/SAST/Miri assurance | **Substantially closed** | `.github/workflows/security-audit.yml:32-88`; `.github/workflows/adversarial-assurance.yml:22-150` |
| GitHub Actions used movable tags | **Closed** | `.github/workflows/*.yml` use 40-character revisions; CI guards the invariant |
| Core release artifacts lacked provenance | **Closed** | `.github/workflows/release.yml:409-557` |
| Classification metadata was a standing unchecked assumption | **Reduced, not eliminated** | registry/codegate metadata checks now exist; individual tool declarations remain part of the TCB |
| Bus factor / external adoption | **Not closed by code** | structural project context, not an implementation defect |

The remaining secure-defaults complaint is now more precise than the baseline's: unattended CLI
execution is fail-closed by default, but interactive and SDK usage still require an explicit OS
isolation decision.

### Flux deployment recommendation

- Strong fit: controlled autonomous coding/operations on Linux or macOS with `sandbox=require`,
  closed network, managed policy, explicit resource limits and curated plugins.
- Strong fit: an embedding host that needs typed authorization/approval/evidence and is prepared to
  configure its own OS isolation and quota floor.
- Conditional fit: authenticated service deployments using multiple engines/replicas plus shared
  rate/spend enforcement.
- Poor fit: hostile native plugins, high concurrency through one shared engine, Windows workloads
  requiring Flux-provided OS isolation, or embedders expecting sandboxing to follow automatically
  from `auto_approve(true)`.

## Direct comparison

| Decision | Better fit | Why |
| --- | --- | --- |
| Trusted local interactive coding | **Pi** | More mature UX, provider breadth, session ergonomics and extension freedom; its permissive security model matches the explicitly trusted setting. |
| Unattended work on a valuable repository | **Flux** | Mandatory dispatch envelope, bounded iteration, guarded IO, secret redaction and fail-closed unattended CLI sandbox posture. |
| Embedding a lightweight loop in Node/Bun | **Pi** | Smaller conceptual contract and broad injection/RPC surfaces. Add policy and containment externally. |
| Embedding a policy-bearing execution substrate | **Flux** | Authorization and approval are runtime types and cannot be disabled; custom tools share the envelope. |
| Provider experimentation | **Pi** | Broader verified built-in registry and easier same-language provider extensions. |
| Durable authored workflows / replay / audit evidence | **Flux** | Flux-Lang, event/value stores, replay/fork and dispatcher evidence are first-class runtime concepts. |
| First-class delegated agents | **Flux** | Authority-narrowed sub-agent roles and budgets are shipped; Pi deliberately leaves orchestration to extensions/packages. |
| Maximum extension freedom | **Pi** | In-process TypeScript can replace nearly every layer. This is also why it is not a security boundary. |
| Untrusted third-party native extensions | **Neither** | Pi packages and Flux plugin binaries are both trusted code. Use a container/VM or a separately hardened remote worker. |
| High concurrency from one shared harness instance | **Undetermined** | Flux has a verified engine-wide turn mutex; Pi parallelizes tool calls, but this review did not establish concurrent-session throughput for either harness. |
| Security/release assurance | **Flux** | Both are disciplined; Flux currently has the denser adversarial CI and attested-release posture. |
| Existing community and extension ecosystem | **Pi** | Official repository state shows vastly more users/contributors. This lowers integration discovery risk, not execution risk. |

## Ecosystem snapshot

On 2026-08-01, the official GitHub API reported Pi at 81,617 stars and 10,079 forks, versus zero
stars/forks for the newly published Flux repository. Pi's latest release was `v0.83.0` (2026-07-29);
Flux's was `v0.48.0` (2026-08-01). These values support the ecosystem/maturity conclusion only.
They do not validate safety, correctness or benchmark quality.

Sources: [Pi repository API][pi-api], [Pi v0.83.0][pi-release], [Flux repository API][flux-api],
[Flux v0.48.0][flux-release].

## Open questions

- How do the two harnesses compare on the same model, repository, task corpus and approval posture
  for success rate, latency, token cost and operator interventions?
- What are cold-start time, steady-state memory and sustained turn/tool throughput on the same host?
- Can Pi's policy hooks support a maintained, non-optional safety profile without extension-order or
  direct-host-API bypasses, or is external containment the permanent design answer?
- Can Flux remove the per-engine turn mutex without weakening immutable turn identity or session
  validity, and what real throughput target requires it?
- What external audits, incident history and independently operated production deployments exist for
  either project?
- How much of Pi's public extension ecosystem and Flux's plugin pack receives ongoing security
  review rather than install-time trust?

## Bottom line

Choose **Pi** when the human and host are trusted and you value a polished, provider-rich,
hackable coding environment above an internal security boundary.

Choose **Flux** when the runtime must remain the authority after the model, prompt and workflow have
spoken—especially for unattended effects, multi-principal services, evidence trails and bounded
delegation.

If untrusted native code enters the process boundary, choose neither without external isolation.

[pi-loop-boundary]: https://github.com/earendil-works/pi/blob/aa0ec808b970db31822e07835a46647cb51d9d66/packages/agent/src/agent-loop.ts#L277-L312
[pi-tool-prepare]: https://github.com/earendil-works/pi/blob/aa0ec808b970db31822e07835a46647cb51d9d66/packages/agent/src/agent-loop.ts#L600-L663
[pi-tool-concurrency]: https://github.com/earendil-works/pi/blob/aa0ec808b970db31822e07835a46647cb51d9d66/packages/agent/src/agent-loop.ts#L489-L553
[pi-providers]: https://github.com/earendil-works/pi/blob/aa0ec808b970db31822e07835a46647cb51d9d66/packages/ai/src/providers/all.ts#L86-L127
[pi-sessions]: https://github.com/earendil-works/pi/blob/aa0ec808b970db31822e07835a46647cb51d9d66/packages/coding-agent/src/core/session-manager.ts#L30-L80
[pi-agent-cancel]: https://github.com/earendil-works/pi/blob/aa0ec808b970db31822e07835a46647cb51d9d66/packages/agent/src/agent.ts#L306-L323
[pi-session-cancel]: https://github.com/earendil-works/pi/blob/aa0ec808b970db31822e07835a46647cb51d9d66/packages/coding-agent/src/core/agent-session.ts#L833-L854
[pi-sdk]: https://github.com/earendil-works/pi/blob/aa0ec808b970db31822e07835a46647cb51d9d66/packages/coding-agent/src/core/sdk.ts#L38-L85
[pi-ci]: https://github.com/earendil-works/pi/blob/aa0ec808b970db31822e07835a46647cb51d9d66/.github/workflows/ci.yml#L13-L42
[pi-audit]: https://github.com/earendil-works/pi/blob/aa0ec808b970db31822e07835a46647cb51d9d66/.github/workflows/npm-audit.yml#L1-L31
[pi-pins]: https://github.com/earendil-works/pi/blob/aa0ec808b970db31822e07835a46647cb51d9d66/scripts/check-pinned-deps.mjs#L40-L62
[pi-lifecycle]: https://github.com/earendil-works/pi/blob/aa0ec808b970db31822e07835a46647cb51d9d66/scripts/generate-coding-agent-shrinkwrap.mjs#L224-L253
[pi-permissions]: https://github.com/earendil-works/pi/blob/aa0ec808b970db31822e07835a46647cb51d9d66/README.md#L37-L45
[pi-default-tools]: https://github.com/earendil-works/pi/blob/aa0ec808b970db31822e07835a46647cb51d9d66/packages/coding-agent/src/core/sdk.ts#L245-L251
[pi-paths]: https://github.com/earendil-works/pi/blob/aa0ec808b970db31822e07835a46647cb51d9d66/packages/coding-agent/src/core/tools/path-utils.ts#L44-L50
[pi-bash]: https://github.com/earendil-works/pi/blob/aa0ec808b970db31822e07835a46647cb51d9d66/packages/coding-agent/src/core/tools/bash.ts#L82-L103
[pi-env]: https://github.com/earendil-works/pi/blob/aa0ec808b970db31822e07835a46647cb51d9d66/packages/coding-agent/src/utils/shell.ts#L122-L133
[pi-before-tool]: https://github.com/earendil-works/pi/blob/aa0ec808b970db31822e07835a46647cb51d9d66/packages/coding-agent/src/core/agent-session.ts#L460-L488
[pi-project-trust]: https://github.com/earendil-works/pi/blob/aa0ec808b970db31822e07835a46647cb51d9d66/packages/coding-agent/src/core/project-trust.ts#L24-L95
[pi-extension-load]: https://github.com/earendil-works/pi/blob/aa0ec808b970db31822e07835a46647cb51d9d66/packages/coding-agent/src/core/extensions/loader.ts#L412-L490
[pi-package-trust]: https://github.com/earendil-works/pi/blob/aa0ec808b970db31822e07835a46647cb51d9d66/packages/coding-agent/README.md#L404-L408
[pi-loop]: https://github.com/earendil-works/pi/blob/aa0ec808b970db31822e07835a46647cb51d9d66/packages/agent/src/agent-loop.ts#L163-L274
[pi-stop-hook]: https://github.com/earendil-works/pi/blob/aa0ec808b970db31822e07835a46647cb51d9d66/packages/agent/src/types.ts#L207-L217
[pi-agent-options]: https://github.com/earendil-works/pi/blob/aa0ec808b970db31822e07835a46647cb51d9d66/packages/agent/src/agent.ts#L96-L121
[pi-server-experimental]: https://github.com/earendil-works/pi/blob/aa0ec808b970db31822e07835a46647cb51d9d66/packages/server/README.md#L1-L4
[pi-server-migration]: https://github.com/earendil-works/pi/blob/aa0ec808b970db31822e07835a46647cb51d9d66/packages/server/README.md#L43-L57
[pi-server-config]: https://github.com/earendil-works/pi/blob/aa0ec808b970db31822e07835a46647cb51d9d66/packages/server/src/server.ts#L30-L60
[pi-server-auth]: https://github.com/earendil-works/pi/blob/aa0ec808b970db31822e07835a46647cb51d9d66/packages/server/src/server.ts#L222-L259
[pi-unix]: https://github.com/earendil-works/pi/blob/aa0ec808b970db31822e07835a46647cb51d9d66/packages/server/src/transports/unix/listener.ts#L225-L240
[pi-api]: https://api.github.com/repos/earendil-works/pi
[pi-release]: https://github.com/earendil-works/pi/releases/tag/v0.83.0
[flux-api]: https://api.github.com/repos/codewandler/flux
[flux-release]: https://github.com/codewandler/flux/releases/tag/v0.48.0
