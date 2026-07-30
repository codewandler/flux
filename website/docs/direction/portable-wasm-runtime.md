---
title: Running Flux in a WebAssembly sandbox
description: A proposed second execution substrate for Flux — compile the runtime to WebAssembly so a program submitted by someone else can be executed with no ambient authority at all.
---

# Running Flux in a WebAssembly sandbox

:::danger This is a direction, not a feature
**Nothing on this page is implemented.** It is a published design record so the reasoning is
reviewable before any code exists, and so nobody builds against it by accident. There is no Wasm
build of Flux, no browser runtime, and no way to submit a program for sandboxed execution today.

Tracked as epic **C-268** with stories C-269 – C-273. The full design record lives in the repository
at [`docs/designs/portable-wasm-runtime.md`](https://github.com/codewandler/flux/blob/main/docs/designs/portable-wasm-runtime.md).
:::

## The problem it addresses

Flux runs `.flux` on one substrate: a native process with the operating system's ambient authority,
confined after the fact by an OS sandbox and by Flux's own authorization → approval → guarded-IO
envelope. That is the right shape for a Flux **you** installed and trust.

It does not answer a different question: **what if the program came from someone else?** If a customer
submits a `.flux` program for us to execute, that program is arbitrary code aimed at our capability
set. The honest options today are "run it in a container we manage" or "don't".

## Why WebAssembly specifically

A WebAssembly module starts with **no ambient authority at all**. No filesystem, no network, no
process spawning, no clock — not "restricted", but absent, unless the embedder explicitly hands the
module an import.

That is the same posture Flux's [plugin host](/docs/plugins/authoring) already takes on purpose:
capabilities are deny-by-default and scoped to what a manifest declares. The difference is where the
enforcement lives. For plugins, Flux constructs the restriction around a subprocess that *does* have
its own authority. For a Wasm module, the runtime provides it, and there is nothing to construct.

A second benefit is reach. The same module runs in a browser, in an edge worker, and in any embedder
with a Wasm runtime — so running a Flux program would not require installing Flux.

## The design decision

"Compile `.flux` to WebAssembly" can mean two things, and the design picks one:

| | Generate a module per program | **Port the interpreter** (chosen) |
|---|---|---|
| Flux semantics | Re-implemented in a compiler backend | Reused exactly — one engine |
| `retry`, `parallel`, budgets, approval | Must be regenerated correctly every time | Already correct, already tested |
| Long-term risk | Two implementations that must agree forever | None by construction |

Flux's core claim is that the *runtime* decides what happens, not the program. A per-program compiler
pushes those decisions into generated code, which points the wrong way. So the plan is to make the
existing engine portable and feed it the parsed program — the parser and AST layer already performs no
IO, which is the part that makes this plausible at all.

## The property that carries the security argument

> The guard runs **outside** the sandbox. The module never receives a raw capability.

This is easy to get backwards, and getting it backwards produces something that looks safe and is not.
If the embedder exports a general `fetch(url)` and the module is *expected* to call a safety check
first, then a hostile program simply does not call it. The module controls its own control flow; a
check it can skip is not a check.

So the imports a module receives are narrow, already-decided operations rather than primitives:

- not `fetch(url)`, but a request against a named endpoint where the **host** resolves the endpoint,
  applies the private-network egress guard, connects to the exact vetted address, and injects
  credentials the module never sees
- not `open(path)`, but scoped reads where both the permitted scope and the requested path are reduced
  to their physical identity before being compared, so a symlink cannot leave the scope
- a host-supplied clock, because an unrestricted clock is both a side channel and a fingerprint

Credentials never cross the boundary. That is the same reasoning that lets Flux run plugins without
exposing host secrets to them.

## What the sandbox does not do

Stated plainly, because this is where such designs usually stop being honest:

- **It does not bound resources.** A module can loop forever or allocate until the embedder dies.
  CPU, memory and wall-clock limits are the embedder's job and have to be built — they are not
  inherited from the sandbox.
- **It does not prevent authorized exfiltration.** If a program is granted access to a destination, it
  can send whatever it holds there. The boundary constrains *which* destinations, never *what* is sent.
- **It does not replace the OS sandbox.** For the Flux you run yourself, the existing fail-closed
  requirement still applies. This is an additional inner boundary for untrusted programs — defence in
  depth, not a substitute.
- **It does not address side channels.** Timing and memory-growth observation are out of scope.

## Open questions

Genuinely undecided, and listed so the gaps are visible rather than implied:

- **Would a first version run models at all?** A submitted program that calls an AI operation needs a
  provider, and the credential has to stay on the host. A deliberately **model-free** first version —
  deterministic authored flows only — is much smaller and much easier to defend, and covers most of the
  submitted-program use case.
- **Which flavour of WebAssembly**: a plain module with hand-written imports, or the Component Model
  with typed, versioned interfaces.
- **Determinism as a product.** A module plus a recorded set of import responses is a reproducible run,
  which is close to what [the Time Machine](/docs/agent/time-machine) and the
  [Agent Lab](/docs/sdk/agent-lab) already provide. Whether those unify is worth asking later.

If you have a use case that depends on this, the design record in the repository is the place to argue
with it while it is still cheap to change.
