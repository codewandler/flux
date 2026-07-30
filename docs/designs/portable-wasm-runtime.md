# Design: a portable Flux runtime — WebAssembly as a second execution substrate

**Epic:** C-268 · **Status:** proposed (nothing implemented)

## Problem

Flux executes `.flux` on one substrate: a native Rust process with ambient OS authority, confined
after the fact by an OS sandbox (bubblewrap / `sandbox-exec`) and by flux's own
authorization → approval → guarded-IO envelope. That works for a flux the operator installed and
trusts.

It does not answer the case this epic exists for: **someone else's `.flux`, submitted to us, executed
by us.** Today the only honest answers are "run it in a container we pay for and manage" or "don't".
A submitted program is arbitrary code against our capability set; the OS sandbox is a coarse,
platform-specific, fail-closed-if-missing layer (C-262), and it is the *host's* confinement, not a
per-program one.

WebAssembly changes the default. A Wasm module has **no ambient authority at all** — no syscalls, no
filesystem, no network, no clock — unless the embedder hands it an import. That is the same posture
flux's plugin host already takes deliberately ("deny-by-default and manifest-scoped"), except the
runtime enforces it instead of a policy layer we wrote. The prize is a substrate where
*untrusted-by-default* is the starting point rather than something we have to construct.

The secondary prize is reach: the same module runs in a browser, in an edge worker, and in any
embedder with a Wasm runtime — so a `.flux` program becomes something a customer can run locally,
in a docs playground, or in their own infrastructure, without installing flux.

## Decision

**Port the interpreter; do not write a Flux-to-Wasm code generator.**

"Compile `.flux` to WebAssembly" has two readings, and they are not close in cost:

| | **(a) Codegen** — lower each `.flux` program to its own Wasm module | **(b) Portable interpreter** — compile the existing engine to Wasm, feed it the AST |
|---|---|---|
| Semantics | Re-implemented in the backend: `retry`, `parallel`, `until`, context budgets, approval gating, effect declarations | Reused exactly — one engine, one meaning |
| Divergence risk | Two implementations of Flux semantics that must agree forever | None by construction |
| Safety envelope | Must be regenerated per program, and is only as good as the generator | The same envelope code that CI already gates |
| Delivers the goal | Yes | Yes |

(b) delivers everything the problem statement asks for, and (a) additionally signs us up for a second
Flux semantics with no user-visible benefit. Flux's value is that the *runtime* decides, not the
program; a per-program compiler pushes decisions into generated code, which is the wrong direction.

So: `flux-lang` parses to an AST (it already does, IO-free at L0), and a Wasm build of the engine
evaluates that AST, calling out through imports for everything it cannot do itself.

## The load-bearing invariant

> **The guard runs outside the sandbox. The module never receives a raw capability.**

This is the whole security argument, and it is easy to get backwards. If a host exports `fetch(url)`
and the module is expected to call `guard_url` first, then a submitted program simply does not call it
— the module controls its own control flow, and no amount of in-module checking binds an adversary who
wrote the module. Every check that matters must sit on the **host** side of the import boundary, where
the submitted program cannot reach it.

Concretely, imports are **narrow, already-decided operations**, not primitives:

- not `fetch(url)` but `http_get(endpoint_ref)` — where the host resolves the ref, applies
  `guard_url_scoped` (which already resolves hostnames to IPs and blocks
  private/loopback/link-local/ULA/CGNAT ranges), pins the connection to the vetted address (C-256/C-257),
  and injects credentials the module never sees
- not `open(path)` but scoped reads through the equivalent of `read_file_scoped`, which reduces both
  the scope anchor and the requested path to physical identities before matching
- not `now()` unbounded but a host-supplied clock, because a clock is a side channel and a
  fingerprint

This mirrors the plugin host exactly, and that precedent is the reason to trust the shape: a plugin
"may only run programs / read secret keys / reach HTTP hosts / dial targets its manifest declares",
and because the plugin process is env-cleared it "cannot read host secrets via `std::env`". A Wasm
module is a strictly better version of that boundary — the plugin is a subprocess with its own
ambient authority that we constrain, while the module has none to begin with.

## What Wasm does not give us

Stating this plainly, because "it runs in a sandbox" is where this kind of design usually stops being
honest:

- **No resource bound.** A Wasm module can spin forever and allocate until the embedder dies. CPU and
  memory limits are the embedder's job (fuel/epoch interruption, a memory ceiling, a wall-clock
  deadline). Unbounded-loop protection is *not* inherited from the sandbox and must be a named
  acceptance criterion, not an afterthought.
- **No protection against authorized exfiltration.** If we grant a program egress to a host, it can
  send whatever it holds to that host. The Wasm boundary constrains *which* destinations, never
  *what* is sent. Data-flow limits remain flux's policy problem, exactly as they are natively.
- **No secret hygiene by itself.** It follows only from never passing secrets across the boundary.
- **Not a substitute for the OS sandbox.** For the flux *we* run, C-262's fail-closed posture still
  applies. Wasm is a second, inner boundary for *submitted* code — defence in depth, not a
  replacement.
- **Side channels stay open.** Timing and memory-growth observation are not addressed here and are
  out of scope.

## What has to change in the tree

Two concrete blockers, both measured rather than assumed:

1. **`flux-system::System` is a concrete struct, not a trait** (`crates/flux-system/src/lib.rs:1077`).
   Every guarded operation is an inherent method on it, so there is no seam a Wasm backend can
   implement. `flux-plugin`'s `SystemSource` abstracts *which* `System` you get, not *what a System
   is*. This is the main piece of work, and it is a wide-but-shallow refactor: the trait already has
   its shape dictated by the existing method set.
2. **`flux-flow` binds `rusqlite` directly** — but only in `src/state.rs`, **1 of 22 files**. The
   engine's persistence is already isolated to one place, so this is an extraction rather than a
   rewrite — though not a mechanical one: that file is ~940 lines with 25 public functions and 12
   `rusqlite` references, and five of them match `rusqlite::Error::QueryReturnedNoRows`
   **structurally** (`:287`, `:339`, `:517`, `:554`, `:606`). A port therefore has to give the trait
   its own "no such row" representation, or that error variant leaks straight through the abstraction
   and the port is portable in name only.
   **`flux-events` does NOT need this work** — it already has the seam: `trait EventBackend` at
   `crates/flux-events/src/store/mod.rs:255`, with `mod sqlite` and `mod postgres` behind it. Its
   residual `rusqlite` mentions are test-only. That trait is also the in-repo precedent for what a
   backend port looks like here, so C-270 should follow it rather than invent a shape.
   *(Both counts above were corrected after the first measurement: `ls src/*.rs` missed
   subdirectories, and the claim that flux-events needed the same treatment was simply wrong.)*

Encouragingly, the codebase idiom is already trait ports — `flux-runtime` exposes `LoopHost`,
`Spawner`, `DispatchLedger`, `SkillLoader`, `SurfaceSink` and more — so this is continuing an existing
pattern rather than introducing one. And the layering map makes the target explicit: `flux-lang` is
**L0, no IO**, so the parser and AST are portable today.

## Shape

```
┌─ embedder (browser, edge worker, our service) ──────────────────┐
│  the trust boundary is HERE — every check lives on this side     │
│                                                                  │
│  · resolves endpoint refs, injects credentials                    │
│  · guard_url_scoped + address pinning                             │
│  · scoped path identity                                           │
│  · fuel / memory / deadline limits                                │
│  · records evidence + audit                                       │
│                                                                  │
│   ▲ narrow, already-guarded imports        │ AST in, result out   │
│   │                                        ▼                      │
│  ┌─ Wasm module: the portable Flux engine ──────────────────────┐ │
│  │  flux-lang (parse/AST, L0)                                   │ │
│  │  the evaluation core: control flow, retry, parallel, budgets  │ │
│  │  effect declarations + authorization requirements             │ │
│  │  NO ambient authority. NO secrets. NO syscalls.               │ │
│  └──────────────────────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────────────────────┘
```

## Open questions

These are genuinely open and should be settled by their stories, not here:

- **Model calls.** A submitted program that calls `ai_*` needs a provider, and the credential must
  stay host-side. Is a host-mediated inference import in scope, or is v1 deliberately **model-free**
  — deterministic authored flows only? Model-free is a much smaller and much more defensible first
  target, and most of the customer-submitted-code use case does not need inference.
- ~~**Which Wasm flavour.**~~ **Settled by C-271: `wasm32-unknown-unknown` with a hand-written ABI.**
  See [§ Settled: the Wasm flavour](#settled-the-wasm-flavour) below.
- ~~**`tokio` in the portable core.**~~ **Settled by C-271: feature-gate it, and use no async runtime
  at all.** See [§ Settled: what replaces `tokio`](#settled-what-replaces-tokio) below.
- **Determinism as a product feature.** A Wasm module plus a recorded set of import responses is a
  perfectly reproducible run — which is very close to what the Time Machine (C-43) and the Agent Lab
  cassette already do. Whether to unify them is worth asking, but not in v1.
- **Trust in the AST path.** If the embedder parses and the module evaluates, the parser is host-side
  and untrusted input is the AST; if the module parses, the parser is inside. The latter is a smaller
  host attack surface and is probably right, but it means shipping the parser in the module.
  *(C-271 built the module the second way — the ABI takes `.flux` **text** and the module parses it —
  but only as the shape that was simplest to prove, not as a decision. The question stays open.)*

## Settled: the Wasm flavour

**`wasm32-unknown-unknown` with a hand-written ABI. Not WASI, not the Component Model — and the
reason is a security argument, not a toolchain one.**

WASI is a *capability set*: `wasm32-wasip1`/`wasip2` exist to give a module a filesystem, a clock,
an environment, sockets and stdio. The whole premise of this epic is that a submitted program starts
with **none** of those and receives only narrow, already-decided operations (§ "The load-bearing
invariant"). Choosing a WASI target means starting from a preopened world and then arguing about
which parts of it to withhold — the fail-open direction. `wasm32-unknown-unknown` starts from
nothing, so every authority the module has is one we typed out by hand and can see in the module's
import section.

That is not rhetoric: the C-271 module's import section is **empty**, and
`the_portable_module_declares_no_imports` in `crates/flux-lang/tests/wasm_parity.rs` asserts it. A
model-free flow genuinely needs no clock and no syscall, so "no ambient authority" is structural
here rather than a policy we enforce. When C-272 adds the first guarded imports, each one becomes a
visible line in that section and the same test is the review surface.

The Component Model's real advantages — a typed, versioned, WIT-described ABI — are advantages we
want *for the host-import boundary* (C-272), and they are additive later: a component can be built
over the same portable core once that boundary has a settled shape worth describing in WIT. Adopting
it now would mean designing the interface and adopting `wasm-tools`/`cargo-component` before we know
what the interface is. The hand-written ABI is three functions
(`flux_alloc` / `flux_dealloc` / `flux_eval`) and cost nothing to throw away.

**Revisit when** the host-import set stabilizes (after C-272/C-273), or when an embedder we care
about speaks components natively.

## Settled: what replaces `tokio`

**Nothing. The portable core runs the interpreter's future on a bounded poll loop with a no-op
waker, and `tokio` is reduced to a compile-time dependency.**

Two separate problems hid behind this question:

1. **Compiling.** The workspace pins `tokio` with `features = ["full"]`, and `net` pulls `mio`, which
   refuses outright: *"This wasm target is unsupported by mio. If using Tokio, disable the net
   feature."* This was the **only** thing stopping `flux-lang` from building for `wasm32` — the fix
   is a target-gated dependency in `crates/flux-lang/Cargo.toml` giving the wasm build
   `["sync", "macros", "rt", "time"]` and leaving the native build byte-identical. No source change,
   no `cfg` in the interpreter.
2. **Running.** The interpreter is `async`, but on its pure path it is a single non-concurrent future
   whose only suspension point is `tokio::task::yield_now`, which re-wakes immediately. So it needs
   no reactor at all: `block_on` in `crates/flux-lang/examples/portable/core.rs` is ~20 lines of
   `Future::poll` with a no-op waker and a poll budget. No threads, no timer wheel, no
   `Instant::now`, no `SystemTime::now` — each of which is either absent or panicking on
   `wasm32-unknown-unknown`.

The poll budget is the honest part. A program that needs a *timer* (`loop every_ms`, `timeout`,
`throttle`, `debounce`) or an `await` stays `Pending` forever under this executor, and the budget
turns that hang into the error *"the portable core has no reactor: this program needs a host import
to make progress"*. That is the right failure: **a clock is a host import** (§ "The load-bearing
invariant" — "not `now()` unbounded but a host-supplied clock"), and the portable core must not
invent one. Wiring the clock is C-272's business; C-273's wall-clock deadline replaces the poll
budget with a real one.

## Non-goals

- Replacing the native runtime. Native stays the primary substrate.
- Replacing the OS sandbox for flux's own execution (C-262 stands).
- A browser IDE or playground UI. The runtime is the deliverable; a playground is a possible consumer.
- Wasm *plugins*. The plugin protocol is a separate seam with its own versioned line (C-143); whether
  plugins later become Wasm components is a different question and is not decided here.
- Data-flow / exfiltration policy. Unchanged by this epic, and unsolved either way.

## Stories

| ID | Story |
|---|---|
| C-268 | Epic tracker |
| C-269 | A `System` trait: give guarded IO a seam a non-native backend can implement |
| C-270 | Extract the engine's state store behind a port, off `rusqlite` |
| C-271 | Prove the portable core compiles to `wasm32` and evaluates a model-free flow |
| C-272 | The host-import ABI, with every guard on the host side — and a test that proves a module cannot bypass one |
| C-273 | Embedder resource limits: fuel, memory ceiling, wall-clock deadline |
