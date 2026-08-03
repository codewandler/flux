# Publishing to crates.io — runbook

The publish closure is **34 crates**: the `flux-sdk` + `flux-providers` SDK surface, the plugin
authoring surface (`flux-datasource`, `flux-credentials`, `flux-plugin`, `flux-plugin-protocol`), and the
standalone contracts and capabilities that external consumers depend on directly — `flux-a2a`,
`flux-audio`, `flux-config`, `flux-capabilities`, and `flux-web` (the web pack: `http.request`,
`web.fetch`, `web.crawl`, `browser.*`) — plus the reusable app/channel host closure (`flux-auth`,
`flux-lsp`, `flux-app`, `flux-server`, `flux-channels`). Publishing in dependency order means every
dependent resolves its deps from the index.
The authoritative list is the `CRATES` array in `scripts/publish-crates-io.sh` — this doc mirrors it.

> Status: **live.** The SDK closure (20 crates) shipped in v0.9.3. The plugin authoring surface (4
> crates: datasource, credentials, plugin, host-kit) ships with v0.9.4; host-kit left the closure in
> 0.29.0 (C-146) and `flux-plugin-protocol` joined it. All vanity-prefixed
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
(`flux-cli`, `flux-tui`, `flux-eval`, `flux-codegate`) stay **bare and path-only** — not published.

## 2. The closure & topological publish order

**34 crates.** Each crate's dependencies precede it (machine-verified — see the guard noted
below). This list mirrors the `CRATES` array in `scripts/publish-crates-io.sh` in order — keep the two in sync. Publish the
`codewandler-*` package for each:

```
1.  flux-core          ← root (no flux-* deps)
2.  flux-audio          (pure leaf — no flux-* deps)
3.  flux-a2a            (pure leaf — no flux-* deps)
4.  flux-markdown       (pure leaf — no flux-* deps; enters via flux-skill/flux-agent)
5.  flux-datasource     (pure leaf — no flux-* deps; enters via flux-plugin + host-kit)
6.  flux-policy         (before flux-spec: C-141's FlowEffect move took `Action` with it)
7.  flux-secret
8.  flux-evidence
9.  flux-spec           (→ policy)
10. flux-plugin-protocol (→ spec, evidence, datasource)  — the plugin WIRE CONTRACT, on an
    independent semver line (C-143); publishes from this workspace, unlike host-kit
11. flux-config         (→ core, policy, evidence; required by flux-runtime)
12. flux-skill          (→ markdown)
13. flux-system
14. flux-provider       (the abstraction, singular)
15. flux-credentials    (→ core, provider)
16. flux-pg             (→ core)   — the sole sqlx owner; flux-events' optional `postgres` backend
17. flux-lang
18. flux-events         (→ core, lang; optional → flux-pg behind `postgres`)
19. flux-runtime        (→ config)
20. flux-tools
21. flux-cognition
22. flux-plugin         (→ datasource, credentials, runtime, system, spec, secret, evidence, core)
23. flux-capabilities   (→ core, datasource, pg, plugin, runtime, secret, spec, system)
24. flux-flow           (→ lang, events, runtime, provider, skill, evidence, system, spec, secret, core)
25. flux-agent          (→ flow, markdown, skill, tools, runtime, provider, events, evidence, core)
26. flux-orchestrate    (→ agent, flow, …)
27. flux-providers      (→ core, provider, credentials)  — the concrete clients (plural); precedes
    flux-sdk because the SDK's optional `providers` feature (D-153) depends on it
28. flux-sdk            (→ orchestrate, agent, flow, cognition, …; optional: providers, credentials)
29. flux-web            (→ core, runtime, spec, system, plugin, markdown, datasource, evidence)  — the web pack
30. flux-auth           (→ policy)
31. flux-lsp            (→ flow, capabilities, web, runtime, system, …) — library import remains
    `flux_lsp`; its distributed executable remains `flux-lsp`
32. flux-app            (→ agent, orchestrate, flow, cognition, runtime, …)
33. flux-server         (→ app, auth, lsp, sdk, web, …)
34. flux-channels       (→ app, server, system, credentials, flow, …) — reusable inbound channel host
```

**Not in this list: `host-kit`.** Since C-146 it ships with the plugin pack from
`.github/workflows/release-plugins.yml`, not with a flux release — it sits on the independent
protocol line, so a flux cut cannot change its version. It depends on `flux-plugin-protocol`, which
IS published here, so the pack workflow refuses to publish until that version is live on crates.io.

`flux_codegate::tests::publish_script_covers_a_registry_resolvable_closure` checks this list's
membership **and its order** against real `cargo metadata` dependencies, for both publishers.
Ordering matters because `cargo publish` rejects a crate whose dependency is not yet on the index —
and it does so only after the release tag is pushed.

Why `flux-pg` is in the closure: crates.io requires **every** dependency — including optional ones — to
be published. `flux-events` (which `flux-agent` needs) has an optional `postgres` feature that pulls
`flux-pg`, so `flux-pg` must ship too. `sqlx` is only compiled when a consumer enables `postgres`, so
default SDK users pay nothing for it.

Why `flux-plugin` + `flux-plugin-protocol` are in the closure: third-party plugins (outside this repo)
depend on `host-kit`, which pulls `flux-plugin-protocol` and `flux-datasource`. Without publishing
these, every external plugin needs a git-SSH dep + deploy key. `host-kit` itself lives in the nested
`plugins/` workspace and ships with the pack (C-146), so it is no longer published from here.

## 3. Version metadata (already applied)

Every closure crate carries `version` alongside `package`+`path` in `[workspace.dependencies]`. The
refactors that pulled `flux-markdown`, `flux-orchestrate`, and `flux-pg` into the closure had left them
path-only; all three now carry a `version` (the last packaging blockers). `scripts/cut-release.sh` keeps
these in lockstep with `[workspace.package].version` on every release. The plugin surface
(`flux-datasource`, `flux-credentials`, `flux-plugin`) was added to the closure in v0.9.4. The
protocol-line crates (`flux-plugin-protocol`, `flux-spec`, `flux-policy`, `flux-secret`,
`flux-evidence`, `flux-datasource`, and `host-kit`) carry explicit independent versions that the
release script deliberately does NOT touch (C-143). D-249 moved protocol and host-kit to 2.x while
the serde-compatible framed marker remained v1.

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

To release, cut the version (`scripts/cut-release.sh <ver>`), then use the build-once sequence printed
by the script:

```sh
git push origin main
gh workflow run release.yml --ref main -f version=X.Y.Z
# Wait for the candidate run to succeed; its summary SHA must equal `git rev-list -n1 vX.Y.Z^{}`.
git push origin vX.Y.Z
```

The manual run builds all five cargo-dist targets for that exact main SHA and retains the immutable
workflow artifacts plus a version/SHA/run receipt for 14 days. The matching tag run verifies the
receipt and promotes those artifacts without recompiling, while retaining the normal public-release
asset verification. The tag simultaneously starts this crates.io publish workflow.

If no successful, unexpired candidate exists at the tag's exact SHA, the binary workflow emits a
prominent warning and performs the legacy full build. It never searches by version alone. A malformed
or mismatched receipt fails closed before release creation; re-running promotion remains safe because
GitHub Release uploads use `--clobber` and the crates publisher skips versions already present.

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

> ⚠️ **Pass `--latest=false`.** GitHub ranks `/releases/latest` by **`published_at`**, not by the tag
> date or by semver — so publishing an old tag's Release *now* makes that old version "latest",
> which is precisely the N-001 bug this runbook exists to repair. Backfilling `v0.9.3` on 2026-07-29
> flipped `/releases/latest` from `v0.33.0` to `v0.9.3` until it was repaired with
> `gh release edit v0.33.0 --latest`. Note this is invisible in the release's `created_at`, which
> harmlessly inherits the tag date and so looks correct.

```sh
run_id=<failed Release workflow run id>
tag=vX.Y.Z
rm -rf /tmp/flux-release-backfill
gh run download "$run_id" --repo codewandler/flux --pattern 'artifacts-*' --dir /tmp/flux-release-backfill
find /tmp/flux-release-backfill -name '*-dist-manifest.json' -delete
gh release create "$tag" --repo codewandler/flux --target "$(git rev-list -n1 "$tag")" \
  --latest=false \
  --title "${tag#v}" --notes "Backfilled release for $tag."
find /tmp/flux-release-backfill -type f -print0 |
  xargs -0 gh release upload "$tag" --repo codewandler/flux
scripts/verify-github-release.sh --repo codewandler/flux "$tag"
scripts/check-release-tags.sh --repo codewandler/flux   # the latest pointer is still the newest version
```

First check the run actually *built* — only backfill a tag whose platform jobs all succeeded and
whose `host` job was the step that failed. A run that died in `plan` or in the build matrix never
produced a complete asset set, and a Release with partial assets advertises downloads that do not
exist. Those tags belong in `ALLOWED_WITHOUT_RELEASE` in `scripts/check-release-tags.sh` instead:

```sh
gh run view "$run_id" --repo codewandler/flux --json jobs \
  --jq '.jobs[] | select(.conclusion != "success") | "\(.name) -> \(.conclusion)"'
```

**Audit the whole fleet** (every `vX.Y.Z` tag has a Release; `/releases/latest` is the newest one).
This runs in CI on every push to `main`, and is the standing guard against N-001 — `verify-github-release.sh`
only ever inspects the single tag being cut, so it cannot see a tag whose workflow died before
reaching it:
```sh
scripts/check-release-tags.sh
```

Run mid-cut it prints a yellow `NOTE ... still being published` for the tag whose Release workflow has
not finished yet, and still passes (C-252) — that line is the audit telling you it deferred on
evidence, not a warning you need to act on. The same tag turns into a red `FAIL` naming the run's
conclusion as soon as that workflow finishes without publishing a Release, so a dead cut is never
silently forgiven.

- **Irreversible.** A published `name@version` can never be reused — only yanked.
- If a mid-sequence publish fails, fix and re-run — already-published crates are skipped.

## 6. Post-publish

- `cargo owner --add <github-team-or-user> <crate>` on each crate.
- Confirm docs.rs built each crate (`https://docs.rs/codewandler-flux-sdk`, …).
- Smoke-test from a scratch project: `cargo add codewandler-flux-sdk codewandler-flux-providers`, then
  run the README quick-start (imports stay `use flux_sdk::…` / `use flux_providers::…`).

## 7. Follow-on (out of scope here)

The remaining product binaries and internal tooling (`flux-cli`, `flux-tui`, `flux-eval`,
`flux-codegate`) stay path-only and unpublished — a separate, later decision.
