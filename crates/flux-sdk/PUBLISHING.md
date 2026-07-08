# Publishing `flux-sdk` (+ `flux-providers`) to crates.io — runbook

`flux-sdk` sits at the top of a **20-crate publish closure**: publishing it means publishing every crate
it transitively depends on, in dependency order. This release also publishes **`flux-providers`** (the
concrete LLM clients) so `cargo add codewandler-flux-sdk codewandler-flux-providers` yields an agent that
can actually talk to a provider out of the box.

> Status: **prepared, validated, and automated.** The closure is vanity-prefixed `codewandler-flux-*`
> (§1), every packaging blocker is closed (§3), both workspaces build and the full gate is green. The
> actual publish runs **in CI** on a version tag (§5) — the only manual step is configuring one secret.

## 1. Naming — resolved: `codewandler-flux-*` vanity prefix

`flux-core` is TAKEN on crates.io by an unrelated crate (newest `0.5.2`) and every crate depends on it,
so the bare `flux-*` names cannot be used. **Decision: vanity-prefix the whole closure
`codewandler-flux-*`** (all 20 names verified available). Already applied in the manifests. Per crate:

```toml
# crates/flux-core/Cargo.toml
[package]
name = "codewandler-flux-core"   # the crates.io name
[lib]
name = "flux_core"               # the import path stays `flux_core` — zero source churn
```
```toml
# root Cargo.toml [workspace.dependencies] — alias key stays `flux-core`, so every
# `flux-core.workspace = true` consumer and all `use flux_core::…` are untouched:
flux-core = { package = "codewandler-flux-core", version = "0.9.3", path = "crates/flux-core" }
```

The nested `plugins/` workspace carries the same `package =` key for the closure crates it references by
path (`flux-secret`, `flux-spec`). `flux-codegate` strips the `codewandler-` prefix before layer
classification. Non-closure crates (`flux-cli`, `flux-app`, `flux-server`, `flux-tui`, `flux-plugin`,
`flux-datasource`, `flux-a2a`, `flux-credentials`, `flux-auth`, `flux-capabilities`, `flux-channels`,
`flux-audio`, `flux-config`, `flux-eval`, `flux-codegate`) stay **bare and path-only** — not published.

## 2. The closure & topological publish order

**20 crates.** Each crate's dependencies precede it (verified against `cargo tree`). The list lives in
`scripts/publish-crates-io.sh`; keep the two in sync. Publish the `codewandler-flux-*` package for each:

```
1.  flux-core          ← root (no flux-* deps)
2.  flux-markdown       (pure leaf — no flux-* deps; enters via flux-skill/flux-agent)
3.  flux-spec
4.  flux-policy
5.  flux-secret
6.  flux-evidence
7.  flux-skill          (→ markdown)
8.  flux-system
9.  flux-provider       (the abstraction, singular)
10. flux-pg             (→ core)   — the sole sqlx owner; flux-events' optional `postgres` backend
11. flux-lang
12. flux-events         (→ core, lang; optional → flux-pg behind `postgres`)
13. flux-runtime
14. flux-tools
15. flux-cognition
16. flux-flow           (→ lang, events, runtime, provider, skill, evidence, system, spec, secret, core)
17. flux-agent          (→ flow, markdown, skill, tools, runtime, provider, events, evidence, core)
18. flux-orchestrate    (→ agent, flow, …)
19. flux-sdk            (→ orchestrate, agent, flow, cognition, …)
20. flux-providers      (→ core, provider)  — the concrete clients (plural)
```

Why `flux-pg` is in the closure: crates.io requires **every** dependency — including optional ones — to
be published. `flux-events` (which `flux-agent` needs) has an optional `postgres` feature that pulls
`flux-pg`, so `flux-pg` must ship too. `sqlx` is only compiled when a consumer enables `postgres`, so
default SDK users pay nothing for it. `flux-providers`' own normal closure is just `flux-core` +
`flux-provider` + itself. `flux-datasource` is **not** in the closure and is deliberately excluded.

## 3. Version metadata (already applied)

Every closure crate carries `version` alongside `package`+`path` in `[workspace.dependencies]`. The
refactors that pulled `flux-markdown`, `flux-orchestrate`, and `flux-pg` into the closure had left them
path-only; all three now carry a `version` (the last packaging blockers). The stray `version` on the
non-closure `flux-datasource` was dropped. `scripts/cut-release.sh` keeps these in lockstep with
`[workspace.package].version` on every release.

## 4. Pre-flight (no registry writes)

Only the **leaves** can be fully validated before their deps are on crates.io — a non-leaf's package step
resolves its `flux-*` deps against the index (empty until we publish), so it reports "no matching
package" until then. That's expected, not a blocker.

```sh
cargo publish --dry-run -p codewandler-flux-core       # leaf: fully meaningful (name now free)
cargo publish --dry-run -p codewandler-flux-markdown   # leaf: no flux-* deps
scripts/publish-crates-io.sh --dry-run                 # runs the whole ordered list in --dry-run
```

## 5. The actual publish — automated in CI

Pushing a `vX.Y.Z` tag triggers **`.github/workflows/crates-io.yml`**, which runs
`scripts/publish-crates-io.sh` (the §2 order, idempotent — an already-published crate@version is
skipped, so a failed run is re-runnable).

**One-time setup — the required secret:** add **`CARGO_REGISTRY_TOKEN`** under
*Settings → Secrets and variables → Actions* on the `codewandler/flux` repo. It is a crates.io API token
(https://crates.io/settings/tokens) from an account that can publish the `codewandler-flux-*` names.
Without it the job fails fast with a clear message and nothing is published.

To release: cut the version (`scripts/cut-release.sh <ver>`), then `git push --follow-tags origin main`.
The tag fans out to both the binary `Release` workflow and this crates.io publish.

**Manual fallback** (from a maintainer machine with a token):
```sh
cargo login                      # or: export CARGO_REGISTRY_TOKEN=…
scripts/publish-crates-io.sh     # same ordered, idempotent loop
```

- **Irreversible.** A published `name@version` can never be reused — only yanked.
- If a mid-sequence publish fails, fix and re-run — already-published crates are skipped.

## 6. Post-publish

- `cargo owner --add <github-team-or-user> <crate>` on each crate.
- Confirm docs.rs built each crate (`https://docs.rs/codewandler-flux-sdk`, …).
- Smoke-test from a scratch project: `cargo add codewandler-flux-sdk codewandler-flux-providers`, then
  run the README quick-start (imports stay `use flux_sdk::…` / `use flux_providers::…`).

## 7. Follow-on (out of scope here)

The rest of the platform (`flux-cli`, `flux-app`, `flux-server`, `flux-tui`, `flux-plugin`,
`flux-datasource`, `flux-credentials`, `flux-auth`, `flux-a2a`, `flux-capabilities`, `flux-channels`,
`flux-audio`, `flux-config`, `flux-eval`) stays path-only and unpublished — a separate, later decision.
