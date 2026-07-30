---
id: C-268
title: "A portable Flux runtime — WebAssembly as a second execution substrate (epic)"
pillar: Core
status: ready
priority: 4
epic: portable-wasm-runtime
design: docs/designs/portable-wasm-runtime.md
note: "EPIC — run someone else's .flux inside a sandbox that starts with NO authority; port the interpreter, never a Flux-to-Wasm codegen. Blockers measured: flux-system::System is a struct not a trait; flux-flow binds rusqlite in 1/22 files"
---

# A portable Flux runtime — WebAssembly as a second execution substrate (epic)

## Goal

Give flux a second execution substrate where **untrusted-by-default is the starting point**, so a
`.flux` program submitted by someone else can be executed by us without granting it our ambient
authority. A Wasm module has no syscalls, no filesystem, no network and no clock unless the embedder
hands it an import — the same posture the plugin host constructs by policy, except the runtime enforces
it. The same module then runs in a browser or an edge worker, so a program becomes something a customer
can run without installing flux.

## Acceptance

- [ ] C-269 lands a `System` trait, so guarded IO has a seam a non-native backend can implement.
- [ ] C-270 extracts the engine's state store behind a port, off the direct `rusqlite` binding.
- [ ] C-271 proves a portable core compiles to `wasm32` and evaluates a **model-free** flow to the
      same result as the native engine — the same `.flux`, the same output, asserted against each
      other rather than against a golden file.
- [ ] C-272 defines the host-import ABI with **every guard on the host side**, and carries the test
      that matters: a module that tries to reach an ungranted destination is refused by the host, and
      a module that simply declines to call a guard cannot thereby escape one.
- [ ] C-273 bounds an embedded run: fuel/epoch, a memory ceiling, and a wall-clock deadline, each with
      a test that an adversarial module hits the limit instead of the embedder.
- [ ] The design's open questions are each closed in writing — in particular whether v1 is
      deliberately model-free, and which Wasm flavour is the target.
- [ ] Public documentation states plainly what the sandbox does and does not buy, including that it
      does not bound resources by itself and does not prevent authorized exfiltration.

## Progress

- (not started — design proposed, nothing implemented)

## Notes

- Design: [portable-wasm-runtime.md](../designs/portable-wasm-runtime.md). Read the
  "What Wasm does not give us" section before writing any acceptance criteria: three of the five
  items there are the kind of thing that gets assumed into a design and then is not true.
- The decision that shapes everything: **port the interpreter, do not write a Flux-to-Wasm code
  generator.** Codegen means a second implementation of Flux semantics — `retry`, `parallel`, budgets,
  approval gating — that must agree with the first one forever, for no user-visible gain.
- Measured, not assumed: `flux-system::System` is a concrete struct at
  `crates/flux-system/src/lib.rs:1077`; `flux-flow` touches `rusqlite` in `src/state.rs` only, 1 of 22
  files (12 references, five of them structural matches on `QueryReturnedNoRows`). `flux-lang` is already **L0 — no IO**, so the parser and AST are portable today.
- This is defence in depth, NOT a replacement for C-262's fail-closed OS sandbox on the flux we run.
