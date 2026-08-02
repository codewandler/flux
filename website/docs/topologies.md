---
sidebar_position: 5
title: Topologies
description: "Every way to run flux — what moves, what stays, what each one costs, and the command that does it."
---

# Every way to run this, and what each costs

flux can run in a lot of shapes: entirely on your laptop, confined by the OS, served over HTTP to a
thin client, embedded in another Rust program, or compiled to WebAssembly. Most of these already
exist, and nothing collected them — so people tend to discover the shape of the product by accident.

This page is a decision aid, not a brochure. Its value is that it says what each option **costs**.

## A topology is four independent choices

Four things decide the shape, and they move **independently** — which is exactly what makes this
confusing without a page:

1. **the runtime** — what *decides*: authorization, the approval prompt, policy.
2. **the system** — where things *happen*: file IO, process spawning, network egress.
3. **the model** — a local provider or a hosted one.
4. **the workspace** — whose files the agent is actually editing.

The rule that makes the first two legible is stated in flux's own substrate design:

> **`flux-runtime` decides whether something may happen. `flux-system` is where it happens.**

They are peers, not layers. That is why "run the agent here and land the effects there" is even
expressible — you are putting that boundary across a network. It is also why the two questions
readers actually get wrong are *where are my files* and *where does the approval prompt appear*: those
follow from axes 4 and 1, and they do not have to move together.

## How to read the status column

This page is only worth having if it is honest about what is built, so every row carries one of
three words:

| word | meaning |
|---|---|
| **ships** | in the released binary. The command shown runs today. |
| **partial** | some of it ships. The row names which part, and what is missing. |
| **proposed** | designed and filed, not built. **No command runs.** The proposed spelling is shown in a plain block, never a runnable one. |

Commands in `sh` blocks on this page are checked against the shipped CLI by a test, so a renamed
flag breaks the build rather than quietly turning a documented topology into a lie.

## At a glance

| Topology | Status | Runtime (decides) | System (does) | Your files | Approval prompt |
|---|---|---|---|---|---|
| [Fully local](#fully-local) | **ships** | your machine | your machine | your machine, unconfined | your terminal |
| [Local, OS-sandboxed](#local-os-sandboxed) | **ships** on Linux and macOS | your machine | your machine, confined | your machine; only the workspace is writable | your terminal |
| [Local runtime, containerized ops](#local-runtime-containerized-ops) | **proposed** | your machine | a container | undecided | your terminal |
| [Local runtime, remote system](#local-runtime-remote-system) | **ships** | your machine | the remote host | the remote workspace is canonical | your terminal, which is the whole point |
| [Served agent, thin client](#served-agent-thin-client) | **ships** | the server | the server | the server's | your choice: over the network (`--remote-approval`), or nowhere (`--yes`) |
| [Embedded in your program](#embedded-in-your-program) | **ships** | your process | your process | your process's working dir | whichever approver you install |
| [Portable WebAssembly](#portable-webassembly) | **partial** — language core only | the embedder | nothing; there is no host authority | none | none; there is nothing to approve |
| [Hosted / multi-tenant](#hosted-multi-tenant) | **partial**, and early | flux-exchange | flux-exchange, HTTP only | not applicable | not applicable |
| [`ssh` to the box](#ssh-to-the-box) | **ships** — it is not a flux feature | the remote host | the remote host | the remote's | your terminal, over the `ssh` session |

The rest of this page is one section per row.

## Fully local {#fully-local}

**Status: ships.**

Everything on your machine: the runtime, the system, the workspace, and — unless you point it at a
hosted provider — your credentials. Nothing leaves the box except the model call.

```sh
flux tui
```

Bare `flux` with no arguments opens the REPL instead; `flux run "…"` does a single headless turn.

- **Where your files are:** your working directory. The agent reads and edits the tree you started it in.
- **Where the approval prompt appears:** your terminal. The TUI raises a modal; the plain CLI asks
  `y/a/N`. Closing the channel counts as a denial.
- **Good for:** everyday coding, and anything where you want to see each effect before it lands.
- **What it costs:** the blast radius is your machine. An interactive run is **not** confined by
  default — see the next row.

## Local, OS-sandboxed {#local-os-sandboxed}

**Status: ships on Linux and macOS.** No Windows backend exists yet.

The same topology, with the effects confined by the operating system: `bubblewrap` on Linux,
Seatbelt (`sandbox-exec`) on macOS. Confinement is applied at a **single spawn choke point**, and
that is enforced rather than asserted — an architecture lint fails the build if any crate spawns a
process outside it.

```sh
flux tui --sandbox
flux run --sandbox "run the test suite"
```

Unattended runs opt in for you. Anything with no human at the keyboard starts at the confined
profile: `--yes` runs, `flux app run --serve`, a `.flux` program in daemon mode, `flux review`, and
`flux plugin call`. Only an explicit `--no-sandbox` (or `FLUX_SANDBOX=off`) turns that back off, and
it prints a warning when it does.

- **Where your files are:** your machine, but the sandboxed child sees only the workspace as
  writable.
- **Where the approval prompt appears:** unchanged — your terminal. Confinement and approval are
  different mechanisms; the sandbox bounds what a *permitted* effect can reach.
- **Good for:** unattended work, untrusted repositories, anything you would not want to run
  unwatched.
- **What it costs:** under the unattended profile the network defaults to **closed**, so anything
  that fetches — a package install, a dependency resolve — fails until you grant it. On a platform
  with no backend, an unattended surface **refuses to start** rather than running unconfined; an
  interactive one runs unconfined.

See [Safety](./agent/safety.md) for the envelope this sits in, and
[Configuration](./reference/config.md) for the `[sandbox]` table.

## Local runtime, containerized ops {#local-runtime-containerized-ops}

**Status: proposed.** No container backend exists in the tree. Filed as
[C-397](https://github.com/codewandler/flux/blob/main/docs/stories/C-397-container-process-backend.md).

The idea: keep the runtime and the approval prompt local, and land process effects in a container
instead of on your host. It is the cheapest way to get a real blast-radius boundary without a
network hop.

⚠ **Do not confuse this with the fleet's worker placement,** which does exist. `AgentRuntime` decides
where a *worker agent* runs; this row is about where a single guarded *operation* lands. Today the
shipped worker runtimes are an OS process and an externally-managed one — neither is a container.

## Local runtime, remote system {#local-runtime-remote-system}

**Status: ships.** Designed in
[remote-agents](https://github.com/codewandler/flux/blob/main/docs/designs/remote-agents.md); the
substrate half is [C-399](https://github.com/codewandler/flux/blob/main/docs/stories/C-399-remote-guarded-io-backend.md).

The one where the agent you drive is here and the system it acts on is there — you approve on your
machine and the effect lands in a container or a microVM somewhere else. The local mode remains the
default; this is an explicit operator-selected execution target, never a mode the model may select.

```sh
# On the execution host. Use a CA-issued certificate in production.
export FLUX_REMOTE_SYSTEM_TOKEN='generate-a-long-random-token'
flux system serve --workspace /srv/project --bind 0.0.0.0:8790 \
  --cert server-cert.pem --key server-key.pem

# On the machine where you run the model and approve effects.
export FLUX_REMOTE_SYSTEM_TOKEN='the-same-token'
flux tui --remote https://worker.example:8790 --remote-ca worker-ca.pem
```

The same `--remote` turn controls are available on `flux run`, `flux tui`, `flux fork`, `flux record`,
and agent-backed app runs. Omit the flag and flux uses the native local system exactly as before.
The bearer value comes only from the environment variable named by `--remote-token-env` (default
`FLUX_REMOTE_SYSTEM_TOKEN`); it is never accepted in the URL or as a literal CLI flag. Publicly
trusted certificates need no `--remote-ca`. A private/loopback endpoint also requires the explicit
global `--allow-private-net` grant.

The v1 daemon serves one canonical workspace over authenticated HTTPS, with authenticated WSS for
managed processes and guarded network streams. The TUI keeps the endpoint and the canonical remote
workspace in its header for the entire session. Port-aware coding operations are available; tools
that still own native-only resources are hidden and refused in remote mode, never run on the local
machine as a fallback.

- **Where your files are:** the **remote workspace is canonical**. Every project-relative read,
  write, discovery operation and process cwd uses that tree. There is **no implicit synchronization**
  with the directory from which the local TUI was started. A local editor sees a different tree
  unless you explicitly mount or attach it to the remote workspace.
- **Where the approval prompt appears:** your terminal. That is the property this topology exists to
  keep, and the reason it is not the same thing as serving an agent.
- **What it costs:** latency on every operation, a deliberately smaller operation catalog while
  native-only integrations are ported, and a new trust question. `Refused`, `Unserved`,
  `Unreachable`, and `Unknown` are structurally distinct. `Unknown` means the daemon accepted an
  effect but cannot prove its terminal result; flux does not automatically retry a mutation.

The daemon stores a bounded delivery ledger at `.flux/remote-system-delivery.json` in the remote
workspace. It contains operation ids, request fingerprints, states, and timestamps—not arguments,
results, or secret values. Replaying the same id cannot execute the effect twice; reusing it for a
different request is refused.

### Which guarantees cross the link

The split below extends the native-substrate contract in [Concepts](./concepts.md#binding-flux-system-without-flux-runtime)
rather than redefining it. "Remote" does not mean "mostly the same":

| Guarantee | Remote classification | Why |
|---|---|---|
| Default-deny authorization and approval | **travels — stays local** | The local `flux-runtime` dispatches and approves before sending an operation. Authorization and approval stay local. |
| Model selection and provider credentials | **travels — stays local** | Model calls are made by the local runtime; the remote system receives no provider key. |
| Workspace path confinement | **becomes the remote system's responsibility** | Only the remote host can resolve symlinks and physical paths against its canonical root. The local side can validate spelling but cannot prove the remote filesystem result. |
| Argv-only spawning, cleared child environment and output caps | **becomes the remote system's responsibility** | The daemon must use its guarded `System`; a report from an arbitrary endpoint is not proof that it did. |
| OS sandbox and egress guard | **becomes the remote system's responsibility** | Bubblewrap/Seatbelt and DNS/IP pinning act where the process or socket is created, which is now remote. |
| Redaction and evidence recording | **travels, with weaker provenance** | The local runtime redacts returned bytes and records them, but the record is **remote reported**, not locally observed. The remote must also redact its own diagnostics and logs. |
| Tool credentials needed by a remote effect | **changes meaning** | The credential store and model credentials stay local, but an **operation-bound secret crosses the encrypted link** when the selected remote operation must use it. The daemon may hold it only in memory for that operation and must never log or persist it. |

The last row corrects an easy but unsafe shorthand: remote mode cannot promise that every credential
value stays on the local machine while also promising that an authenticated process or request runs
on the remote one. The operator must treat the selected remote system as able to observe any secret
explicitly delivered for an approved effect.

For a full remote shell and editor rather than split runtime/effect placement, [`ssh`](#ssh-to-the-box)
remains the simpler option.

## Served agent, thin client {#served-agent-thin-client}

**Status: ships** — both halves, server and client.

Here the *whole* agent runs elsewhere — planning, model calls, tools — and you talk to it. This is
the Docker-CLI shape: a thin client, and the far side does everything.

### Choose the approval posture — it is not chosen for you

flux's envelope is **authorization → approval → guarded IO**. Approval is the only one of those
three with a *human* in it, so which posture it runs under is a decision, and both answers are
legitimate:

```sh
# Ask me, over the network, before each guarded effect.
flux app run --serve 127.0.0.1:8787 --remote-approval

# Do not ask. Constrain through policy, the sandbox floor and budgets instead.
flux app run --serve 127.0.0.1:8787 --yes
```

There is no default. Starting a served agent without one of those flags is refused, because
guessing is how someone ends up unattended without meaning to.

**`--remote-approval`** parks every guarded effect and waits for a human:

```sh
curl -s localhost:8787/approvals
# { "approvals": [ { "id": "ap_…", "fingerprint": "…", "tool": "write",
#                    "subjects": ["report.txt"], "mutating": true, … } ],
#   "timeout_secs": 120 }

curl -s -X POST localhost:8787/approvals/ap_… \
  -H 'content-type: application/json' \
  -d '{"fingerprint":"…","decision":"allow"}'
```

Three properties worth knowing before you build a client against it:

- **An effect nobody answers is denied.** The wait is `FLUX_APPROVAL_TIMEOUT_SECS` (default 120),
  and there is no "wait forever" — an unbounded wait is a wedged turn, not a decision.
- **You must echo the `fingerprint`.** It is the effect in canonical form, and it is what binds your
  `yes` to the effect you were shown; a decision that names a different one is refused with `409`.
  A decision is also single-use, so it cannot be replayed onto the next effect.
- **The operator boundary is one shared token (or open loopback).** The routes sit behind the
  server's auth, and an unauthenticated non-loopback bind is refused outright. Principal auth is
  refused for this posture: one global queue would otherwise let one principal answer another's
  effects. Anyone holding the shared token can make the agent do anything it is authorized to do.

**`--yes`** never asks. That is not safety switched off: authorization policy, the mandatory
fail-closed sandbox floor on this surface, and the resource budgets are still doing the
constraining, and for high-autonomy work — research, security hardening, long exploration — that is
often the *better* design. Interrupting an agent for every effect is not caution if nobody is going
to read the prompts.

Either way, treat the endpoint's authentication as a real boundary and do not serve one onto a
network you do not control.

:::note What this looked like before
Until this landed, no approver in flux spoke over a network — every one was local (the terminal
prompt, the TUI modal, the sub-agent approver). A served agent therefore had no posture to pick: it
was `--yes` or it did not start. **If you are running a served agent today, you have been running
the unattended posture.** That may well still be the right one for your job — but it is now
something you choose rather than something that was chosen for you.
:::

### The rest of the surface

Either posture exposes a `/.well-known/agent-card.json` discovery card, `POST /a2a` JSON-RPC with
`message/send` and `message/stream`, and a session REST subtree (`POST /sessions`,
`GET /sessions/{id}`, `POST /sessions/{id}/messages`, plus an SSE stream). See
[HTTP API](./agent/http-api.md) and [A2A](./agent/a2a.md).

Connect to one:

```sh
flux a2a http://127.0.0.1:8787 "summarize the open bugs"
```

With no prompt it opens an interactive session against the remote agent instead.

- **Where your files are:** the server's. The agent edits the tree *it* was started in; your local
  files are not in the picture at all.
- **Where the approval prompt appears:** wherever your client puts it. Under `--remote-approval`
  the server parks each effect at `/approvals` and it is your client's job to show it to a human;
  under `--yes` nobody is asked.
- **Good for:** giving a team or another agent access to one configured agent; agent-to-agent work.
- **What it costs:** the model choice and the credentials live on the server, and under
  `--remote-approval` every guarded effect costs a network round trip and a human. If what you
  wanted was "my terminal's approval prompt, someone else's blast radius", this is still the wrong
  row — that is [local runtime, remote system](#local-runtime-remote-system).

## Embedded in your program {#embedded-in-your-program}

**Status: ships.**

Another Rust program takes flux as a library — `codewandler-flux-sdk` — and drives the agent
in-process. There is no CLI in the picture.

```rust
let client = flux_sdk::Client::builder()
    .model("anthropic/opus")
    .build(provider, ".")?;
let out = client.run("Summarize the README").await?;
```

- **Where your files are:** whatever root your program hands the client.
- **Where the approval prompt appears:** wherever you put it. You install the approver, so it can be
  a terminal prompt, your own UI, a policy function, or nothing.
- **Good for:** building a product on flux rather than around it.
- **What it costs:** ⚠ **the SDK does not inherit the CLI's sandbox floor.** A library has no argv to
  classify, so `auto_approve(true)` does **not** imply confinement — the embedded client reads the
  ambient `FLUX_SANDBOX` setting, which is off unless you set it. If you auto-approve in an embedded
  program, ask for the sandbox explicitly.

See the [SDK overview](./sdk/overview.md).

## Portable WebAssembly {#portable-webassembly}

**Status: partial**, and narrower than "flux in the browser". Filed as
[C-268](https://github.com/codewandler/flux/blob/main/docs/stories/C-268-portable-wasm-runtime-epic.md).

What ships: the Flux-Lang **evaluation core** compiles to `wasm32-unknown-unknown` behind a
three-function ABI, and a parity test proves the wasm build agrees with the native one on the same
source.

```sh
bash scripts/build-portable-wasm.sh
```

What does not ship: everything that touches the world. The portable core is handed **no host
authority at all** — its operation catalogue is empty and every dispatch returns a denial. So
literals, expressions, formatting, field access and control flow evaluate; tools, the model, IO and
the agent loop do not exist there. Serving the guarded port through host imports is the next step and
has not started.

- **Where your files are:** there are none.
- **Where the approval prompt appears:** nowhere — there is nothing to approve, by construction.
- **Good for:** evaluating Flux-Lang inside an embedder that cannot spawn a process.
- **What it costs:** it is not an agent. Do not plan on it as one yet.

## Hosted / multi-tenant {#hosted-multi-tenant}

**Status: partial**, and early. This is a separate project,
[flux-exchange](./ecosystem.md), which holds credentials and knows about tenants.

flux itself never needs it. The charter line the ecosystem design enforces:

> **flux must never *require* flux-exchange.**

What exists today (v0.11.0): a loopback service with OIDC sign-in, a per-tenant connection surface, a
file-backed credential store, agent-principal minting, and an invoke endpoint that runs one
catalogue operation. Two facts decide whether you can plan on it:

- **A minted agent token authenticates nothing yet.** Nothing binds the agent store to the identity
  port, so a token you hand an agent is refused by every guarded route exactly as an unknown value
  would be.
- **A multi-tenant deployment refuses to execute on the host.** HTTP is shareable because the effect
  leaves the machine; process spawning, container exec and raw sockets consume the host's own
  identity and filesystem, so a shared deployment serves only HTTP and remote runtimes and refuses
  the rest. "Runs ops per tenant" today means **HTTP connector operations**, not agent execution.

Channels, leases, stored workflows and execution records are described in the design and are not
built.

## `ssh` to the box {#ssh-to-the-box}

**Status: ships** — though it is not a flux feature, and that is the point of listing it.

Install flux on the remote machine and run it there over `ssh`. This is a legitimate answer, it costs
nothing, and for a lot of people it is the *right* answer.

```sh
ssh you@remote-host
```

…then any of the local topologies above, on that box.

- **Where your files are:** the remote's — all of them, consistently. There is no synchronisation
  problem, which is the failure mode that sinks most "run it over there" tooling.
- **Where the approval prompt appears:** your terminal, over the `ssh` session.
- **Good for:** a beefier machine, a machine with the data on it, a throwaway VM.
- **What it costs:** the model call, your API credentials and the agent's whole context live on the
  remote box. Your local editor is not looking at those files. One session per host, and no single
  view across several.

## The model axis is independent

The three axes above say nothing about *which model* answers, and that choice is orthogonal — you
can pair any of these topologies with either kind of provider.

- **Hosted provider** — Anthropic, OpenAI, OpenRouter and friends. Your prompt and context leave the
  machine, whatever the rest of the topology does. A fully local, fully sandboxed run still makes a
  network call to the provider.
- **Local provider** — an OpenAI-compatible server on your own machine, e.g.
  `flux run -m ollama/qwen2.5-coder:7b "…"`. Nothing leaves the box, at the cost of a smaller model
  and your own hardware.

The pairing that surprises people: a **served agent** with a local provider still runs the model on
the *server*, not on yours. Moving the agent moves the model call with it.

See [Providers](./agent/providers.md).

## Choosing

- Coding on your own machine, want to see each effect → **fully local**, add `--sandbox` when you
  step away.
- Unattended or untrusted work → **local, OS-sandboxed**. You already get it on the unattended
  surfaces.
- Someone else's machine should do the work, and you accept losing the approval prompt → **served
  agent**, or just **`ssh`**.
- You want the approval prompt to stay yours while effects land elsewhere → that is **local runtime,
  remote system**. Use `--remote` when you specifically want the local approval and model boundary;
  use `ssh` when moving the whole terminal is simpler.
- Building a product on flux → **embedded**, and set the sandbox yourself.
