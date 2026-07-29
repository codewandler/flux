---
id: A-131
title: "Wire the fleet into a running flux — register the fleet.* ops and bind a WorkBoard from config"
pillar: Agent
status: ready
priority: 28
epic: fleet-coordinator
design: docs/designs/fleet-coordinator.md
note: "SURFACED BY A-117, verified: FleetDispatchTool/A2aSpawner are constructed nowhere outside their own module (only re-exported at flux-orchestrate/src/lib.rs:18), and build_datasources knows only markdown|openapi — so the fleet exists in code and is unreachable from a running flux"
---

# Wire the fleet into a running flux — register the `fleet.*` ops and bind a `WorkBoard` from config

## Goal
A-113 landed the `WorkBoard` port. A-116 landed `fleet.dispatch` / `fleet.status` / `fleet.cancel`
and `A2aSpawner`. Both are real, tested, and **unreachable from a running `flux`**:

- `FleetDispatchTool`, `FleetStatusTool`, `FleetCancelTool` and `A2aSpawner` are constructed
  **nowhere** outside their own module. The only other mention in the workspace is the re-export at
  `crates/flux-orchestrate/src/lib.rs:18`.
- `build_datasources` (`crates/flux-cli/src/execution.rs:178-228`) understands exactly two kinds,
  `markdown` and `openapi`, and builds a *knowledge* backend for both. A `kind` of `jira` errors out;
  a `WorkBoard` cannot be named from configuration at all.

So a `.flux` Program cannot call a fleet op, and cannot declare the board it would operate on. Close
both gaps — this is the wiring that turns two merged stories into a usable feature.

## Acceptance
- [ ] The `fleet.*` ops are registered into the production catalog so a Program can call them.
      **Failing-first test**: assert the registry resolves `fleet.dispatch` — it does not today.
- [ ] Registration follows the existing pack conventions: the op group in `groups.rs`, the
      `builtins_register` expected-name list, **and both op references** (`crates/flux-flow/docs/
      ops-reference.md` *and* `website/docs/language/ops.md` — a registered public op missing from
      either reds `operations_reference_covers_the_registered_public_catalog`).
- [ ] Each registered op's `ToolSpec` is coherent under `flux_spec::metadata_violations`, including
      `semantic_effects` (C-210). Do **not** regress A-116's egress posture: the `worker` endpoint is
      caller-supplied and therefore model-reachable, so it resolves through `guard_url_scoped` before
      any request and `permission_subjects` reports the worker's origin — never `*`, and empty when
      the endpoint cannot be named, which forces approval rather than matching a broad grant.
- [ ] A `WorkBoard` backend is bindable from a declaration, so a Program can name the board it works.
      **Failing-first test**: a Program declaring a board resolves to a `WorkBoard`-backed set of
      generated ops rather than erroring or silently ingesting the file as knowledge.
- [ ] The failure mode that exists today is closed: `kind = "markdown"` currently builds a *knowledge*
      datasource, so a user pointing it at a board would get silent, wrong behaviour rather than an
      error. Whatever naming is chosen, the wrong kind must fail loudly.
- [ ] Standard gate green in both workspaces.

## Progress
- 2026-07-29 — filed from A-117's blocked report; both halves independently re-verified against the
  tree before filing (the missing construction sites and the two known kinds).

## Notes
- **This is the tonight-critical wiring story.** A-117 (the reference Program + end-to-end journey)
  cannot be written until a Program can both *call* a fleet op and *declare* a board.
- Both halves live in `crates/flux-cli/src/execution.rs`, which is why they are one story and not
  two — they would collide as separate concurrent branches.
- ⚠ Coordinate with A-130, which is extending the `WorkBoard` trait with a dispatch-recording seam.
  The generated-op surface (`OPERATIONS`) changes there. Prefer landing after it, or expect to absorb
  the new operation into whatever registration this story adds.
- Naming the board kind is a real decision, not a detail: `markdown` is already taken by the
  knowledge datasource, so a board backed by markdown files (A-114) needs a distinct, unambiguous
  name. Pick it deliberately and say why.
