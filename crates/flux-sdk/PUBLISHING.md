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

### Normal path: merge `main` into `release`

The deliberate release action is merging a pull request from `main` into the protected `release`
branch. Direct pushes to `release` are not the normal path; branch protection should require the PR
and its checks. Open the release PR with:

```sh
gh pr create --base release --head main \
  --title "release: promote main" \
  --body "Merge main into the protected release trigger branch."
```

Merging that PR is the whole release action; its resulting push to `release` starts the workflow.

`.github/workflows/release-flow.yml` runs the live smoke, asks the tool-less release scribe for the
two changelog sections, has the host derive and cut the version, and creates a local release commit
plus annotated tag. Before running Flux unattended, the workflow installs and self-tests bubblewrap;
agentic and served smoke legs remain confined instead of weakening the fail-closed sandbox posture
for CI. Its host-owned promotion step then:

1. stages the cut commit at `refs/heads/release-candidates/vX.Y.Z`;
2. dispatches `.github/workflows/release.yml` from that exact ref and waits for all five cargo-dist
   targets;
3. verifies the candidate's version/SHA/run receipt and exact-SHA lookup;
4. advances `main`, then pushes the annotated tag; and
5. waits for both tag workflows, verifies the public GitHub Release, and only then deletes the
   candidate ref.

The matching tag run verifies the receipt and promotes those artifacts without recompiling, while
retaining the normal public-release asset verification. The tag simultaneously starts the
idempotent crates.io publisher.

Three Actions secrets are required for the default path, with one optional provider override:

- **`ANTHROPIC_API_KEY`** — used only by the default direct-Haiku `FLUX_SMOKE_MODEL` live smoke and
  release-scribe turn. A manual preview without the selected provider key skips, but a push to
  `release` fails loudly without cutting or pushing anything. The direct provider is the default so
  release availability does not also depend on an OpenRouter account balance.
- **`OPENROUTER_API_KEY`** *(optional)* — selected only when a manual dispatch explicitly overrides
  the model with an `openrouter/*` model. The workflow passes provider credentials only to the
  credential probe, live smoke, and Flux flow steps.
- **`RELEASE_TOKEN`** — a fine-grained GitHub PAT scoped to this repository with **Contents: write**.
  It is exposed only to the host promotion step for the candidate/main/tag refs, and to the binary
  workflow for creating or refreshing the Release. A separately authenticated token is required
  because refs pushed with `GITHUB_TOKEN` do not start the tag-triggered workflows.
- **`CARGO_REGISTRY_TOKEN`** — a crates.io API token from an account that can publish the
  `codewandler-flux-*` names. It may be a selected organization Actions secret. Without it the
  crates.io job fails before publishing.

`workflow_dispatch` on `release-flow.yml` is deliberately **not** another publish button. Its
default `apply: false` is a read-only preview; `apply: true` cuts only inside the ephemeral runner as
a rehearsal. Neither manual mode calls the promotion helper or moves a remote ref.

### Manual build-once path

If the automation itself is unavailable, run `scripts/cut-release.sh <version|patch|minor>` from a
clean `main` checkout. It prints this exact sequence; do not advance `main` or push the tag before
the candidate and receipt checks are green:

```sh
tag=vX.Y.Z
sha=$(git rev-list -n1 "$tag^{}")
candidate="release-candidates/$tag"
repo=$(gh repo view --json nameWithOwner --jq .nameWithOwner)
baseline=$(gh run list --workflow release.yml --limit 100 \
  --json databaseId --jq '([.[].databaseId] | max) // 0')

# Make the exact cut commit addressable without moving main.
git push origin "HEAD:refs/heads/$candidate"
gh workflow run release.yml --ref "$candidate" -f version=X.Y.Z

# Wait for the newly dispatched exact-ref/exact-SHA run. The baseline excludes an older run or retry
# while GitHub is still registering this dispatch.
run_id=
until [ -n "$run_id" ]; do
  run_id=$(gh run list --workflow release.yml --event workflow_dispatch \
    --branch "$candidate" --commit "$sha" --limit 20 \
    --json databaseId,event,headBranch,headSha \
    --jq ".[] | select(.databaseId > $baseline and .event == \"workflow_dispatch\" and .headBranch == \"$candidate\" and .headSha == \"$sha\") | .databaseId" \
    | sort -n | head -1)
  [ -n "$run_id" ] || sleep 5
done
gh run watch "$run_id" --exit-status

receipt_dir=$(mktemp -d)
gh run download "$run_id" --name release-candidate-receipt --dir "$receipt_dir"
scripts/release-candidate.sh verify "$receipt_dir/release-candidate.txt" \
  X.Y.Z "$sha" "$run_id"
test "$(scripts/find-release-candidate.sh "$repo" "$sha")" = "$run_id"

# Only the verified commit may now become main and a public version tag.
git push origin HEAD:main
git push origin "$tag"

# Wait for release.yml and crates-io.yml to finish, then:
scripts/verify-github-release.sh --repo "$repo" "$tag"
git push origin --delete "$candidate"
```

The credential used for the candidate, main, and tag pushes must be `RELEASE_TOKEN` (or an
equivalently scoped maintainer credential), not `GITHUB_TOKEN`, so the tag starts both publication
workflows.

If no successful, unexpired candidate exists at the tag's exact SHA, the binary workflow emits a
prominent warning and performs the legacy full build. It never searches by version alone. A malformed
or mismatched receipt fails closed before release creation; re-running promotion remains safe because
GitHub Release uploads use `--clobber` and the crates publisher skips versions already present.

The automated and manual C-251 paths deliberately do not rely on that compatibility rebuild: they
require the exact candidate before pushing the tag. The fallback remains for older/direct tag cuts.

### Failure retention and recovery

Once a candidate ref has been staged, any failed candidate build, ref push, tag workflow, or public
verification leaves `release-candidates/vX.Y.Z` in place at the exact cut SHA. The failure log prints
the SHA and recovery commands. Do not delete that ref or move/recreate the version tag while
investigating.

- **Before the tag exists:** inspect or rerun the candidate workflow at the retained ref. Re-download
  and verify its receipt against the retained SHA. Only after it is green may `main` and then the tag
  be pushed using the manual sequence above.
- **After the tag exists:** never delete or retarget the tag. Rerun the failed `Release` or `crates.io`
  workflow; both paths are idempotent. Verify the public Release, then delete only the matching
  candidate branch.

Useful evidence and cleanup commands:

```sh
tag=vX.Y.Z
candidate="release-candidates/$tag"
git ls-remote origin "refs/heads/$candidate" "refs/tags/$tag^{}"
gh run list --repo codewandler/flux --branch "$candidate"
gh run list --repo codewandler/flux --branch "$tag"
scripts/verify-github-release.sh --repo codewandler/flux "$tag"
git push origin --delete "$candidate"  # only after recovery and public verification are green
```

The verifier confirms the Release object exists and carries the installer scripts, checksum manifest,
platform archives, and provenance attestations that `/releases/latest` users need.

**Registry-only fallback** (from a maintainer machine with a crates.io token):
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
