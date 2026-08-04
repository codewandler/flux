# AGENTS.md — repository contract

This file applies to every coding agent and automation tool working in this repository. It contains
only repository policy that must be known before acting. Product documentation starts at
[README.md](README.md).

## Work contract

- Serve the newest user request first. Do not select backlog work unless the user explicitly asks;
  when they do, use [docs/stories/README.md](docs/stories/README.md).
- For work named by the sibling `flux-roadmap` repository, that repository owns cross-repository
  architecture, dependency order, milestone assignment, and the active tranche. It normally lives
  at `../flux-roadmap`; from a linked worktree, locate it beside the primary Flux checkout. The local
  story's Goal and Acceptance still define done. If they contradict the cross-repository decision,
  amend or supersede the story before implementation instead of treating roadmap prose as acceptance.
- Begin with `git status --short --branch`. Treat existing changes as user-owned. Do not reset,
  discard, rebase, rewrite history, force-push, create a branch/worktree, or commit unless the user
  explicitly asks.
- For a read-only explanation or review, inspect and report without creating project artifacts. For
  story work, its Goal and Acceptance are the definition of done. Create a story from
  [docs/stories/_TEMPLATE.md](docs/stories/_TEMPLATE.md) for substantial untracked work, and record
  non-trivial design decisions in [docs/designs/](docs/designs/).
- Add a failing-first test for behavioral changes. Keep the story/design and changelogs consistent
  with the finished behavior; user-visible changes also belong in `WHATS-NEW.md`.
- Before opening a pull request or entering any publication path, regenerate the committed public
  documentation mirror with `scripts/build-embedded-docs.sh`, commit
  `crates/flux-server/assets/public-docs.zip` when it changes, then run
  `scripts/build-embedded-docs.sh --check` against that committed checkout.
- Make the smallest coherent change, preserve unrelated work, run the relevant gate, and report any
  check that could not be run.

## Architecture and safety

Flux is a Rust agent SDK, harness, and coding agent. Crates are layered L0 through L6 and may depend
only on their own or a lower layer. The authoritative layer map is in `flux-codegate`; the full
design is [docs/architecture.md](docs/architecture.md).

The model is not the runtime. Authored Flux-Lang owns control flow, and every real effect crosses
authorization → approval → guarded IO. Preserve these boundaries:

- All filesystem, process, and network IO in Flux goes through `flux-system`; model-originated
  process execution is argv-only.
- Every in-product tool runs through `flux-runtime::Executor::dispatch`. Do not call tool execution
  directly outside tests.
- Keep secrets out of logs and model-visible output; register them with `flux-secret::Redactor` and
  use secret references rather than literals.
- Keep long-running work cancellable and caller identity immutable for a live turn.
- Tool effects and `permission_subjects` must be accurate. Plugin capabilities remain deny-by-default
  and manifest-scoped.
- Route web egress through the `flux-system` URL guards. Route every OS process through the single
  guarded `System` path with a workspace-pinned cwd, cleared environment, and capped output.
- Keep served HTTP routes authenticated except the documented health/discovery endpoints; never
  permit an unauthenticated non-loopback agent listener.
- Preserve provider-history validity on every termination path: no empty assistant message, split
  tool pair, or adjacent user messages.
- Provider framing errors become counted stream diagnostics; declared provider failures remain
  fatal.

These are release boundaries, not conventions. Never add a bypass.

## Where details live

Read the focused contract before changing these areas:

- Flux-Lang syntax, generated references, or editor mirrors:
  [crates/flux-lang/AGENTS.md](crates/flux-lang/AGENTS.md)
- Plugins or the nested plugin workspace: [plugins/AUTHORING.md](plugins/AUTHORING.md)
- Releases and versioning: [crates/flux-sdk/PUBLISHING.md](crates/flux-sdk/PUBLISHING.md)
- Architecture and crate placement: [docs/architecture.md](docs/architecture.md)
- Product direction: [docs/vision.md](docs/vision.md) and [docs/roadmap.md](docs/roadmap.md)
- Unobserved client-builder wiring and `flux-pin` coverage:
  [docs/designs/unobserved-wiring.md](docs/designs/unobserved-wiring.md)

Flux roles live under `.flux/agents`; reusable skills may live under `.flux/skills`,
`.agents/skills`, or `.claude/skills`. They are optional repository resources, not Flux's universal
harness prompt. Do not put assumptions about a specific host agent, its private tools, or its
runtime UI into this file.

## Verification

Run the narrowest useful checks while iterating, then the repository gate before declaring a code
change complete. Repository scripts and workflows route target-touching Cargo commands through
`scripts/owned-cargo`, whose shared OS lease prevents `task clean` from removing live compiler
output. Direct operator Cargo commands remain valid, but do not overlap them with cleanup of the
same `CARGO_TARGET_DIR`:

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo test -p flux-codegate
```

If process spawning or sandbox posture changed, also run:

```bash
FLUX_BWRAP_BIN=/nonexistent/bwrap cargo test --workspace
FLUX_TEST_SANDBOX_BACKEND=1 cargo test -p flux-cli --test sandbox_backend
```

The `plugins/` directory is a separate workspace. If touched, run its checks with
`--manifest-path plugins/Cargo.toml`, including `cargo fmt --check`.

Golden regeneration is armed only by `FLUX_UPDATE_GOLDEN=1`; regeneration intentionally fails after
writing. Review the diff, then rerun with the variable unset to verify. Never hand-edit generated
Flux-Lang node-kind or prelude tables.
