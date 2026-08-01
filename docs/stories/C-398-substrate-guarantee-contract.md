---
id: C-398
title: "Say what binding flux-system without flux-runtime means"
pillar: Core
status: done
priority: 7
design: docs/designs/execution-substrate.md
epic: execution-substrate
note: "AGENTS.md says every tool runs through Executor::dispatch — true of FLUX, and a reader who finds an out-of-repo consumer bypassing it will reasonably conclude something is broken. Nothing states which guarantees travel with the substrate alone"
---

# Say what binding flux-system without flux-runtime means

## Goal

A consumer may link `flux-system` and bring its own policy engine — that is the point of a published
substrate with an unsealed port. Nothing in the tree says which guarantees such a consumer gets and
which it does not, so the safety story is currently only legible to someone who has read both crates.

## Acceptance

- [x] A contract document (crate-level docs on `flux-system`, and a section reachable from
      `docs/concepts.md`) lists, explicitly and separately:
      - guarantees that travel with `flux-system` alone — path confinement, argv-only execution,
        egress resolution and range blocking, sandbox confinement, env clearing, output capping;
      - guarantees that are `flux-runtime`'s and **do not** travel — default-deny authorization,
        approval, redaction of tool output, evidence.
- [x] It states that a consumer taking only the first set is **supported**, and that assuming the
      second is the failure the document exists to prevent.
- [x] `AGENTS.md`'s *"Every tool runs through `Executor::dispatch`"* is scoped to flux explicitly, so
      it can no longer be read as a claim about every consumer of the substrate.
- [x] The existing `port.rs` answer (what it means to *implement* the port) is cross-linked and not
      duplicated — these are two different questions and both should stay answered once.

## Progress
- Contract written as a "Binding `flux-system` without `flux-runtime`" section at
  `crates/flux-system/src/lib.rs`'s crate root, with a user-facing companion section of the same
  name in `docs/concepts.md` (mirrored to `website/docs/concepts.md`).
- `port.rs` gained a three-line pointer to the crate-root section and repeats none of it; the
  crate-root section likewise points back for the implementor's question.
- `AGENTS.md`'s invariant now reads *"Every tool **in flux** runs through `Executor::dispatch`"* and
  says in the same bullet that binding `flux-system` alone is supported.

## Notes
- Docs-only; no behavioural change. The gate for this story is review, not a test.
- `docs/concepts.md` already carries the peer framing publicly; this is its crate-level companion.
- **Every guarantee was re-checked against source rather than copied from the epic.** Three of the
  six "travels" items needed a qualifier the design's one-line summary did not carry, and each is
  now stated inline:
  - **The OS sandbox is opt-in and off by default.** `System::new` installs `Sandbox::disabled()`
    (mode `Off`); only `System::from_env` / `System::with_sandbox` resolve a real backend. Read as
    an unqualified bullet, "sandbox confinement travels" is the single most likely thing for a
    second consumer to get wrong — it would build a `System` the obvious way and believe it was
    confined.
  - **Egress guarding is a function you call, not an ambient envelope.** `flux-system` performs no
    HTTP; `net::guard_url_scoped` returns a `Url`, and a client that re-resolves that hostname
    reopens the DNS-rebinding TOCTOU. `net::dial_scoped` is safe by construction (it connects
    inside the guard); `guard_url_scoped_pinned` exists for the HTTP case.
  - **Path confinement is bounded by the `Workspace` the consumer constructs** —
    `set_unconfined` (`--allow-all-paths` / `FLUX_ALLOW_ALL`) lifts it outright, and read roots
    widen reads.
  Two smaller ones: the env allow-list is followed by a caller-override slot that wins, and the
  output cap covers *captured* output only (`run_with_env_streamed` inherits stdio and captures
  nothing). The `flux-runtime` "does not travel" list was correct as written, with one refinement
  worth stating — `flux-policy`, `flux-secret` and `flux-evidence` are all L0 crates a second
  consumer may depend on directly, so what does not travel is the *enforcement*, not the mechanism.
- Gates run: `cargo test -p flux-cli --test website_contract` (25 passed),
  `cargo test -p codewandler-flux-lang --test website_in_sync` (5 passed, after regenerating the
  concepts mirror), `cargo test -p codewandler-flux-system` (161 + 1 passed),
  `cargo clippy -p codewandler-flux-system --all-targets -- -D warnings` clean,
  `cargo fmt --all --check` clean. `cargo doc -p codewandler-flux-system --no-deps` emits the same
  21 pre-existing private-item-link warnings and none from the new section.
