# Design: crate consolidation

**Status:** Phase 1 ✅ shipped (`flux-providers`) · Phases 2–4 planned · **Layers:** L1 now, L4/L5/L2/L6 next · **Owner:** Timo

The workspace grew breadth-first — every surface got its own crate — leaving several layers with many
small, tightly-coupled crates that only ever serve one consumer. That is pure overhead: a wider build
graph, more `Cargo.toml` churn, and more names to hold in your head. This initiative cuts the crate
count **without crossing an architectural boundary**, by merging coherent siblings *within the same
layer* (the `flux-codegate` layer map). Each merge keeps the layering lint green and is independently
reviewable.

## Why

- 37 crates is more than the architecture needs. The [`flux-codegate`](../../crates/flux-codegate/src/lib.rs)
  layer map shows the real structure (L0–L6); within a layer, many crates are thin and single-consumer.
- The provider layer was the worst offender: seven L1 crates, five of them tiny wrappers whose only
  external consumer was `flux-cli`.
- Fewer crates ⇒ fewer manifests to keep in lockstep (esp. the publish-closure version dance), a
  smaller build graph, and a clearer mental model.

## Approach — merge within layers

The guiding rule: **only merge crates that already share a layer.** That guarantees no new inner→outer
edge (the lint stays green) and keeps each merge a pure "collapse siblings into modules" operation.
Crates in the **publish closure** (those carrying a `version` in `[workspace.dependencies]`) are left
alone — merging them would change the published API surface.

## Phase 1 — providers → `flux-providers` (✅ shipped)

Merged the five non-published L1 provider crates into one, **keeping `flux-provider` (the published
trait/abstraction) and `flux-credentials` separate**:

| Was (5 crates) | Now (module in `flux-providers`) |
|---|---|
| `flux-messages` | `messages` (shared Anthropic Messages protocol core) |
| `flux-anthropic` | `anthropic` |
| `flux-openrouter` | `openrouter` |
| `flux-ollama` | `ollama` |
| `flux-openai` | `openai` |

L1 went from **7 crates → 3** (`flux-provider`, `flux-providers`, `flux-credentials`); the workspace
went **37 → 33**. Blast radius was minimal: `flux-cli` was the only external consumer, rewired to
`flux_providers::<module>::…` paths. `flux-credentials` stays its own crate deliberately — it is
destined to back credentials for *all* future integrations, not just LLM providers.

> Side note: the move also fixed a pre-existing clippy 1.91 `doc_lazy_continuation` lint that was
> latent in `flux-messages` (present on `main` too).

## Further reductions (tracked, not yet executed)

Each is a within-layer merge of single-consumer (`flux-cli`-only) crates:

- **Phase 2 — L4 extensibility:** `flux-hooks` (214 LOC) + `flux-plugin` (986) → **`flux-plugin`**
  with `hooks`/`plugin` modules. 2→1.
- **Phase 3 — L5 capabilities:** `flux-browser` (135) + `flux-datasource` (229) → **`flux-capabilities`**.
  2→1. `flux-auth` (caller identity) is also L5 but a distinct concern (not web egress / RAG); decide
  at execution whether to fold it in (→ 3→1) or leave it standalone.
- **Phase 4 — L2 / L6 odds & ends:**
  - `flux-context` (332, sessions/context) → fold into `flux-runtime` (same layer L2). −1.
  - `flux-integrations` (102) is an **orphan** — nothing depends on it. Confirm it's live; fold into a
    surface or remove. −1.

**Projected:** 33 → ~28–29 crates.

## Out of scope (do not merge)

- The **16 publish-closure crates** (carry `version` in workspace deps) — merging changes the
  published API surface. `flux-provider` stays separate for exactly this reason.
- **L0 contracts** — kept granular on purpose (see the `flux-codegate` doc comment on deliberate
  L0-leaf placement: `flux-evidence`/`flux-skill`/`flux-config`/`flux-lang`).
- Large **L3 subsystems** (`flux-agent`, `flux-flow`, `flux-eval`, `flux-orchestrate`,
  `flux-cognition`) and large **L6 surfaces** (`flux-sdk`, `flux-tui`, `flux-server`, `flux-app`) —
  distinct, substantial subsystems; no consolidation benefit.
- **`flux-credentials`** — future home for all integration credentials.

## Key files

- New crate: `crates/flux-providers/{Cargo.toml,src/lib.rs,src/messages/,src/anthropic.rs,
  src/openrouter.rs,src/ollama.rs,src/openai.rs}`.
- Workspace manifest: root `Cargo.toml` (`members` + `[workspace.dependencies]`).
- Layer lint: `crates/flux-codegate/src/lib.rs` (the L1 match arm).
- Consumer: `crates/flux-cli/{Cargo.toml,src/main.rs}` (`build_provider` + imports).
- Docs: this file, `.flux/plans/crate-consolidation.md` (impl checklist), `docs/roadmap.md`, `AGENTS.md`.

## Verification

The standing gate, run in the worktree:
- `cargo test -p flux-codegate` — `workspace_respects_layering` proves `flux-providers` is classified
  and no inner→outer edge was introduced.
- `cargo build --workspace` + `cargo test --workspace` — clean (583 tests pass).
- `cargo clippy --workspace --all-targets -- -D warnings` + `cargo fmt --all --check` — clean.
- `grep -rn 'flux_messages|flux_anthropic|flux_openai|flux_openrouter|flux_ollama'` over `crates/` —
  no live code/manifest references remain.
- `cargo run -p flux-cli -- --help` — all eight providers still listed and dispatch through
  `flux-providers`.
