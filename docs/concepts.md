# Concepts

This page defines the words the rest of the documentation uses. Read it before the agent, language,
or security guides.

It is deliberately a **vocabulary**, not a tour. Most confusion about flux comes from two words that
sound interchangeable and are not — *runtime* and *system*, *session* and *lease*, *operation* and
*tool* — so each entry below says what the term means, what it is **not**, and where it lives.

> **Source of truth.** This file is `docs/concepts.md`. `website/docs/concepts.md` mirrors it inside
> a generated block; `crates/flux-lang/tests/website_in_sync.rs` fails on drift. Edit it here.

## The one boundary

**The LLM is not the runtime.** Everything else follows from that sentence.

A flux turn is driven by an authored Flux-Lang outer loop. Inside provider-native typed stages, the
model interprets intent, gathers evidence, and proposes literal calls to visible operations. The host
captures effectful proposals and freezes them into an immutable **action batch**; only an approved
batch is recorded and executed.

The default conversational loop never asks the model for per-turn executable Flux. A separate,
explicit [`op.register`](https://codewandler.github.io/flux/docs/agent/saved-flows#register-an-operation-during-a-turn) operation may
accept exactly one agent-proposed composite operation, and the host analyzes, scopes and guards that
source before installing it. It extends the available vocabulary; it does not replace the authored
outer loop.

Every production operation — evidence reads, approved batches, built-in tools, plugin operations,
sub-agent work, app journeys — crosses the same chain:

```text
authorization → approval → guarded IO
```

There is no trusted shortcut for a model-native call.

---

## What flux is

**Engine** — the deterministic Rust core: the safety envelope, the flow engine, the provider layer,
and the operation catalog. The engine is what makes a run repeatable; it is not a user-facing
product on its own.

**Framework / harness** — the engine plus the machinery you build *with*: providers, tools, skills,
plugins, orchestration. "Harness" is the usual word when the subject is running a model; "framework"
when the subject is building on flux. They name the same thing from two directions.

**SDK** (`flux-sdk`) — the embeddable library form. It assembles the *same* flow engine and the
*same* safety pipeline the CLI uses. The CLI is the reference application built on the SDK, not a
privileged sibling: there is no capability the CLI has that an SDK embedder cannot obtain.

**Agent** — a model plus a loop plus a bounded catalog of operations. Note that in flux an agent is
**not** the unit of execution and not the thing that holds authority: a journey with no agent in it
is an ordinary flux program, and an agent that calls an operation faces exactly the checks a CLI turn
faces. An agent is one node kind, not the runtime.

**Flux-Lang** — the authored workflow language. Small, typed, analyzer-validated, with first-class
`retry`, `throttle`, `saga` and approval gates. It places deterministic control flow *around*
explicit model stages. It is not model output and not a general-purpose language.

---

## What runs: operations and their metadata

**Operation (op)** — the universal callable unit; the system's verbs. Reading a file, running a
test, calling a plugin, posting a Slack message, asking the model to rank items: each is an operation
in one catalog, and each crosses the safety envelope. If something can happen, it is an operation.

**Tool** — an operation *as the model sees it*: a name, a description, and an input schema in the
context window. Every tool is an operation; not every operation is a tool. `expose false` keeps an
operation callable by authored flows while costing the model no context. When these docs say "tool
call" they mean the model-facing face of an operation dispatch.

**Effect** — what an operation does to the world, declared rather than inferred. The set is closed:
`read`, `write`, `network`, `model`, `process`, `browser`, `filesystem`, `local_system`. An
operation with no effects is a pure read.

Do not confuse an effect with a **capability** (below). `write` is an effect; `workspace.write` is a
capability. They are different axes — an effect says what an operation *does*, a capability says
what a caller has been *allowed*. The effect parser rejects a capability name outright.

**Risk** and **idempotency** — declared per operation. `risk` drives approval (destructive
operations are forced to human confirmation even under permissive rules); `idempotency` states
whether repeating the call repeats the effect. Both are part of the operation's contract, so a policy
can be written over them instead of over a hand-maintained list of names.

**Capability** — a named grant that unlocks a set of operations. Capabilities are what a typed intent
stage narrows: the model gets the operations its declared intent justifies, not the whole catalog.

**Action batch** — the immutable, frozen set of effectful calls produced by a turn. It is what
approval approves and what execution executes. Freezing is the point: the thing reviewed is the thing
that runs, and effectful native calls are **re-checked at dispatch**.

**Symbol** — a name bound to an immutable stored value. Flux-Lang flows refer to
symbols such as `src` or `tests` — the formatter's canonical bare spelling, with no sigil. The
runtime owns the value store; the model sees summaries and explicit context packs rather than every
raw output replayed into the prompt. A tool result has two faces for this reason: `content` is the
canonical value that flows into symbols and interpolation, and `view` is an optional model-facing
rendering.

---

## What executes it: `flux-runtime` and `flux-system`

These two are the most-confused pair in the codebase, and they are **peers, not layers** — both sit
at L2 and neither is "inside" the other.

> **`flux-runtime` decides whether something may happen. `flux-system` is where it actually
> happens.**

**`flux-runtime` — the envelope.** One entry point, `Executor::dispatch`, and one chain: permission
check → (if unmatched) approval prompt → execute. It resolves policy, prompts a human when a rule
does not cover the call, redacts secrets from anything it surfaces, and records evidence. It holds
*judgment*. It performs no IO of its own.

**`flux-system` — the guarded substrate.** The only place real filesystem, process, environment and
network IO happens. Every path resolves against a **workspace** root and is refused if it escapes,
lexically (`..`) or via symlink. Process execution is **argv-only** — there is no shell, so nothing
model-authored can inject a shell operator — and every OS process in the entire system, including a
plugin binary, starts at one `build_command`. Network egress resolves hostnames to addresses and
blocks private, loopback, link-local, ULA and CGNAT ranges unless the caller holds a scoped grant.
It holds *mechanism*. It makes no policy decisions.

Why the split is load-bearing, and why merging them would be a mistake: a consumer that wants flux's
execution substrate almost never wants flux's approval model. An automated service has no human at a
terminal to prompt. If judgment and mechanism were one crate, that consumer would have to take both
or reimplement guarded IO — and reimplementing guarded IO is exactly the failure the substrate
exists to prevent. Keeping them apart means a second consumer binds `flux-system` and brings its
*own* policy, without either side weakening.

**Workspace** — a bounded filesystem view: a primary root, optional `@named` roots, and optional
read-only roots that reads may reach but writes may not. Confinement can be lifted explicitly; it is
never lifted implicitly.

**Sandbox** — defense in depth *below* the envelope. An opt-in OS confinement (bubblewrap on Linux,
Seatbelt on macOS) applied at the single spawn choke point, so a spawned process is confined even if
everything above it was satisfied.

**Port** — the guarded operations restated as narrow capability traits, so a non-native substrate can
serve them: a WebAssembly embedder answering through host imports, a remote executor, a test double.
A port makes the *caller* substitutable, not the guard. Optional port operations default to denial,
never to a weaker equivalent — bringing a substrate up starts from "serves nothing".

**Policy** — default-deny authorization: grants over subjects × resources × actions, gated by trust
and scopes, with a usable local default so the agent still works out of the box.

**Approval** — the human gate. Destructive and policy-flagged effects reach it even under permissive
rules, and approval produces a one-shot receipt rather than a standing permission.

**Redactor** — the secret register. Every secret value is registered before it can appear anywhere,
and is scrubbed from tool output, errors and logs. Credentials are references (`secret:env/KEY`),
never literals.

---

## What knows: datasources and evidence

**Datasource** — the knowledge layer: an indexed store of records (workspace documents, integration
data) the agent looks things up in. Operations *do*; datasources *know*.

The two meet cleanly, and this is deliberate: **a datasource is read through operations.** Retrieval
(`search`, `get`, `list`, …) is just more read-only operations in the same catalog, so knowledge
access is governed exactly like action. There is no second permission model for reading.

**Evidence** — the auditable trail a turn produces: intent, selected capabilities, tool calls,
proposed batches, approval events, execution reports, and compaction. It is flushed durably to the
session event log, which is what makes "explain what the agent did and why it was allowed" a query
rather than a reconstruction.

**Replay / fork / diff** — because a run is a deterministic artifact, a recorded run can be re-run
hermetically, forked at any decision, and diffed against another. This is the practical payoff of
the one boundary, and no LLM-as-runtime design can offer it.

**The adaptive typed loop** — a turn is not one blind guess. A typed intent stage narrows the live
operation catalog; exploration uses exact provider-native schemas to gather safe evidence or capture
effects; the host freezes an immutable batch; approval produces a one-shot receipt; and execution
reports return to the same native ledger for local correction. Questions suspend and resume the
authored flow. See [the agent loop](https://codewandler.github.io/flux/docs/agent/agent-loop).

---

## Local-first

flux keeps runtime state and credential storage local by default. When you choose a remote model
provider, flux intentionally sends it the prompt, conversation, and the selected context or workspace
excerpts needed for that call — **local-first is not a zero-egress guarantee**, and this page says so
rather than letting the phrase imply more than it delivers. Provider credentials stay at the host
boundary, and plugin host callbacks are limited to the capabilities declared in their manifests.
Trusted native plugins still carry the [plugin trust boundary](https://codewandler.github.io/flux/docs/security/plugin-trust).

There is no telemetry and no phone-home. What runs on your machine is yours.

Choosing to run [flux-exchange](./ecosystem.md#flux-exchange--the-platform) is a deliberate departure
from this default, and it is deliberate in both directions: credentials move to a service *precisely
so* they stop living in everyone's environment. The local path remains complete and is never removed.

---

## What wakes it: sessions, channels, and leases

Three lifetimes, routinely conflated. They are not variants of each other.

**Session** — a conversation with its history. Event-sourced, resumable, compacted when long. A
session is the unit of *continuity* for an agent. In flux, "session" always means this.

**Channel** — a long-running external surface, **deployment-scoped**. Nobody opened it; an operator
configured it, and it outlives every caller. Most channels are event sources that wake a program on
an external event — a cron schedule, an inbound webhook, a Slack socket — firing under their own
name, so a `trigger` naming that channel routes them to a journey. Channels *push*.

**Room** — a channel with attribution: the only many-party surface, where flux is one participant
among several. Every event names the occupant who caused it, because attribution is not a feature of
a room — it is the precondition for deciding whether to answer at all. Joining a room grants no
authority whatsoever; a room-sourced turn meets the same envelope as a CLI turn.

**Lease** — a **caller-scoped** hold on a stateful resource: opened by a caller, bound to the grant
that opened it, released by that caller or by expiry. An open TCP connection, a `kubectl exec`, a
database transaction. Leases *pull*, and a lease dies with its holder.

> **On the name.** A lease is what other systems would loosely call a "session", and calling it that
> here would collide with the agent session above — the two have opposite lifetimes and opposite
> owners. `lease` is used deliberately so that a sentence about one can never be misread as a
> sentence about the other.

**Trigger** — the binding from a channel event to a journey. A declaration is a bareword name
followed by indented attributes, never a brace-and-equals record:

```flux
trigger on_mention
  on "support.app_mention"
  run answer
```

**Journey** — a durable flow: authored control flow that can suspend, wait, resume, and survive a
restart. An agent may appear in a journey as one node; nothing requires it to.

**Program** — a `.flux` file declaring an application: its agents, channels, datasources, triggers
and journeys together. `flux app run support-bot.flux` serves the whole declaration.

---

## What extends it

**Provider** — a *wire codec × credential* pair, selected as `provider/model`. Adding one is a small
composition. flux is provider-neutral by principle, not by accident: no single vendor may become
load-bearing in the core.

**Plugin** — a subprocess extension speaking the flux plugin protocol over stdio. Its operations are
manifest-scoped with enforced privileges: a plugin may only run programs, read secret keys, reach
HTTP hosts or dial targets its manifest declares, and private or loopback egress additionally needs
an operator grant. Because the plugin process is env-cleared, a plugin cannot read host secrets from
its environment.

**Skill** — a markdown-defined capability pack (instructions, references, scripts) loaded from
`.flux/` and `.claude/` trees. Manual-invocation by default, with an opt-in model-invoked mode.

**Connector** — a *declaration* of what a vendor can do in both directions, compiled into Flux-Lang
operations plus a capability manifest. Connectors are produced by
[flux-connectors](./ecosystem.md#flux-connectors--vendor-descriptions-compiled) and are the answer to "integrating this vendor
should not require writing Rust". A connector is a compiled description; it is not a running thing.

---

## The ecosystem

flux is one repository in a family of three. This page is the vocabulary they all share; the
division of responsibility is [ecosystem.md](./ecosystem.md).

| | |
|---|---|
| **flux** | The engine, the language, the agent, the substrate. What you run on your own machine. |
| **flux-connectors** | Vendor descriptions, compiled. What a vendor can do, and what an operator must supply. No runtime. |
| **flux-exchange** | The platform: a deployed service that holds credentials, terminates channels, runs operations for many principals, and records what happened. |

The boundary test, one sentence each:

- Does it change what happens when an effect executes? → **flux**
- Is it true of the vendor regardless of who runs it? → **flux-connectors**
- Does it require holding a credential or knowing a tenant? → **flux-exchange**

Nothing in that split requires the platform: a `.flux` program loading a connector module on your
laptop is a complete path, and it stays one. flux must never *require* flux-exchange.

---

## Planned, not yet done

Two structural changes are decided and deliberately unstarted. They are recorded here so that
documentation written now does not have to be rewritten when they land.

**The engine moves to `flux-core`.** Today `codewandler/flux` is both the umbrella and the
workspace. The intent is for `codewandler/flux` to become the welcome repository — the front door,
the ecosystem documentation, the getting-started path — and for the crates to move to
`codewandler/flux-core`. No date, no story yet. Documentation should therefore say "the engine" or
"flux-core" when it means the crates, and "flux" when it means the project.

**`flux-system` becomes a published, second-consumer substrate.** Its `port` module already
anticipates a remote executor, and flux-exchange will be the first consumer outside this repository.
That makes the workspace-confined file surface worth stating as a port too — until now its consumers
all held a concrete `System`, so a trait would have been indirection without a seam. A second
consumer is exactly the condition that changes.

---

## Related

- [The agent loop](https://codewandler.github.io/flux/docs/agent/agent-loop) — how intent, exploration, approval, execution and repair compose.
- [Flux-Lang overview](https://codewandler.github.io/flux/docs/language/overview) — the authored language around model boundaries.
- [Infrastructure](https://codewandler.github.io/flux/docs/infrastructure) — how the pieces fit at runtime.
- [Ecosystem](./ecosystem.md) — flux, flux-connectors and flux-exchange.
