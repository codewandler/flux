# Publishing to crates.io — runbook

The publish closure is **24 crates**: the `flux-sdk` + `flux-providers` SDK surface (20 crates), plus the
plugin authoring surface — `flux-datasource`, `flux-credentials`, `flux-plugin`, and `host-kit` (4
crates). Publishing in dependency order means every dependent resolves its deps from the index.

> Status: **live.** The SDK closure (20 crates) shipped in v0.9.3. The plugin authoring surface (4
> crates: datasource, credentials, plugin, host-kit) ships with v0.9.4. All vanity-prefixed
> `codewandler-*` (§1), automated in CI on a version tag (§5).

## 1. Naming — resolved: `codewandler-flux-*` vanity prefix

`flux-core` is TAKEN on crates.io by an unrelated crate (newest `0.5.2`) and every crate depends on it,
so the bare `flux-*` names cannot be used. **Decision: vanity-prefix the whole closure
`codewandler-flux-*`** (incl. `codewandler-flux-host-kit` for the plugin SDK; its original
misnamed `codewandler-host-kit` 0.1.0 is yanked — crates.io names are immutable, so the rename is
a new crate). Already applied in the manifests.
Per crate:

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
path. `flux-codegate` strips the `codewandler-` prefix before layer classification. Non-closure crates
(`flux-cli`, `flux-app`, `flux-server`, `flux-tui`, `flux-a2a`, `flux-auth`, `flux-capabilities`,
`flux-channels`, `flux-audio`, `flux-config`, `flux-eval`, `flux-codegate`) stay **bare and path-only** —
not published.

## 2. The closure & topological publish order

**24 crates.** Each crate's dependencies precede it (verified against `cargo tree`). The list lives in
`scripts/publish-crates-io.sh`; keep the two in sync. Publish the `codewandler-*` package for each:

```
1.  flux-core          ← root (no flux-* deps)
2.  flux-markdown       (pure leaf — no flux-* deps; enters via flux-skill/flux-agent)
3.  flux-datasource     (pure leaf — no flux-* deps; enters via flux-plugin + host-kit)
4.  flux-spec
5.  flux-policy
6.  flux-secret
7.  flux-evidence
8.  flux-skill          (→ markdown)
9.  flux-system
10. flux-provider       (the abstraction, singular)
11. flux-credentials    (→ core, provider)
12. flux-pg             (→ core)   — the sole sqlx owner; flux-events' optional `postgres` backend
13. flux-lang
14. flux-events         (→ core, lang; optional → flux-pg behind `postgres`)
15. flux-runtime
16. flux-tools
17. flux-cognition
18. flux-plugin         (→ datasource, credentials, runtime, system, spec, secret, evidence, core)
19. host-kit            (→ plugin, datasource, spec)  — in the nested plugins/ workspace
20. flux-flow           (→ lang, events, runtime, provider, skill, evidence, system, spec, secret, core)
21. flux-agent          (→ flow, markdown, skill, tools, runtime, provider, events, evidence, core)
22. flux-orchestrate    (→ agent, flow, …)
23. flux-sdk            (→ orchestrate, agent, flow, cognition, …)
24. flux-providers      (→ core, provider)  — the concrete clients (plural)
```

Why `flux-pg` is in the closure: crates.io requires **every** dependency — including optional ones — to
be published. `flux-events` (which `flux-agent` needs) has an optional `postgres` feature that pulls
`flux-pg`, so `flux-pg` must ship too. `sqlx` is only compiled when a consumer enables `postgres`, so
default SDK users pay nothing for it.

Why `flux-plugin` + `host-kit` are in the closure: third-party plugins (outside this repo) depend on
`host-kit` which transitively pulls `flux-plugin` and `flux-datasource`. Without publishing these, every
external plugin needs a git-SSH dep + deploy key. `host-kit` lives in the nested `plugins/` workspace and
is published via `--manifest-path` (see `scripts/publish-crates-io.sh`).

## 3. Version metadata (already applied)

Every closure crate carries `version` alongside `package`+`path` in `[workspace.dependencies]`. The
refactors that pulled `flux-markdown`, `flux-orchestrate`, and `flux-pg` into the closure had left them
path-only; all three now carry a `version` (the last packaging blockers). `scripts/cut-release.sh` keeps
these in lockstep with `[workspace.package].version` on every release. The plugin surface
(`flux-datasource`, `flux-credentials`, `flux-plugin`) was added to the closure in v0.9.4; `host-kit`
inherits its version from the plugins workspace (`0.1.0`).

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

**Binary Release workflow secret:** add **`RELEASE_TOKEN`** under
*Settings → Secrets and variables → Actions* on the `codewandler/flux` repo. It must be a fine-grained
GitHub PAT scoped to this repo with **Contents: write**. This is required even though
`.github/workflows/release.yml` requests `contents: write`: tag-triggered `GITHUB_TOKEN` release
creation has produced `HTTP 403: Resource not accessible by integration`, leaving a tag without a
GitHub Release object. The workflow now fails fast if `RELEASE_TOKEN` is missing, creates or refreshes
the release idempotently, then runs:

```sh
scripts/verify-github-release.sh vX.Y.Z
```

That verifier confirms the Release object exists and carries the installer scripts, checksum manifest,
and platform archives that `/releases/latest` users need.

**Manual fallback** (from a maintainer machine with a token):
```sh
cargo login                      # or: export CARGO_REGISTRY_TOKEN=…
scripts/publish-crates-io.sh     # same ordered, idempotent loop
```

**Backfill a missing binary Release** (tag exists, GitHub Release does not):
```sh
run_id=<failed Release workflow run id>
tag=vX.Y.Z
rm -rf /tmp/flux-release-backfill
gh run download "$run_id" --repo codewandler/flux --pattern 'artifacts-*' --dir /tmp/flux-release-backfill
find /tmp/flux-release-backfill -name '*-dist-manifest.json' -delete
gh release create "$tag" --repo codewandler/flux --target "$(git rev-list -n1 "$tag")" \
  --title "${tag#v}" --notes "Backfilled release for $tag."
find /tmp/flux-release-backfill -type f -print0 |
  xargs -0 gh release upload "$tag" --repo codewandler/flux
scripts/verify-github-release.sh --repo codewandler/flux "$tag"
```

- **Irreversible.** A published `name@version` can never be reused — only yanked.
- If a mid-sequence publish fails, fix and re-run — already-published crates are skipped.

## 6. Post-publish

- `cargo owner --add <github-team-or-user> <crate>` on each crate.
- Confirm docs.rs built each crate (`https://docs.rs/codewandler-flux-sdk`, …).
- Smoke-test from a scratch project: `cargo add codewandler-flux-sdk codewandler-flux-providers`, then
  run the README quick-start (imports stay `use flux_sdk::…` / `use flux_providers::…`).

## 7. Follow-on (out of scope here)

The rest of the platform (`flux-cli`, `flux-app`, `flux-server`, `flux-tui`, `flux-auth`, `flux-a2a`,
`flux-capabilities`, `flux-channels`, `flux-audio`, `flux-config`, `flux-eval`) stays path-only and
unpublished — a separate, later decision.
