---
id: C-651
title: "Sandboxed is a selectable peer backend"
pillar: "Core"
status: done
epic: first-class-hosts
areas: [flux-system]
design: first-class-hosts
note: "Decision 0018 rule 3: confinement as a peer ExecutionSystem, not only a spawn-time modifier; codegate census entry under review"
---

# Sandboxed is a selectable peer backend

## Goal

Confinement becomes a selectable peer. Today the sandbox (bubblewrap/Seatbelt,
`crates/flux-system/src/sandbox.rs`) is a modifier applied inside the native `System`'s single
spawn choke point. A `sandboxed` backend kind resolves to an `ExecutionSystem` implementation that
composes the native system with the sandbox, so host selection, posture floors and Decision 0018
rule 8's confined default can name it like any other backend.

## Acceptance

- [x] `backend = "sandboxed"` resolves to a peer `ExecutionSystem` implementation that passes the
      `flux-codegate` backend census through a reviewed ALLOW entry.
- [x] On a platform with no usable confinement backend the binding fails closed at resolution
      (`Require` semantics), never degrading silently; a test proves the refusal face.
- [x] The existing `--sandbox`/`--no-sandbox` modifier path is byte-for-byte unchanged, and a
      posture `SandboxFloor` may force selection of the sandboxed backend.
- [x] The peer backend's `SubstrateIdentity` reports its confinement truthfully.


## Comments

- Review round-2 residual: an SDK embedder that pins an AlreadyConfined sandbox via System::with_sandbox and calls SandboxedSystem::from_env inherits the FLUX_SANDBOXED marker without the CLI's audit disclosure (which lives in apply_sandbox_env). No in-tree caller does this; settled by an L2-side disclosure if the peer is ever exposed through flux-sdk.
