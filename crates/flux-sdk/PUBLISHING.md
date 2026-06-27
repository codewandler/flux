# Publishing `flux-sdk` to crates.io — runbook

`flux-sdk` is the top of a **16-crate publish closure**: publishing it means publishing every crate it
transitively depends on, in dependency order. This document is the pre-flight + step-by-step. It does
**not** publish anything — running the real `cargo publish` is a deliberate, irreversible act (crates.io
versions are permanent; you can only *yank*) that needs your crates.io token.

> Status: **prepared, blocked on a name decision.** The version metadata, packaging, and ordering are
> done and validated. A name-availability check surfaced one blocker (see §1) that needs a decision
> before the first publish.

## 1. ⚠️ Name availability — read this first

A crates.io check on `2026-06-27` of the 16 closure crates found:

| Name | Status |
|---|---|
| **`flux-core`** | **TAKEN** — an unrelated crate already owns it (newest `0.5.2`) |
| `flux-spec`, `flux-policy`, `flux-secret`, `flux-evidence`, `flux-skill`, `flux-system`, `flux-provider`, `flux-session`, `flux-lang`, `flux-runtime`, `flux-tools`, `flux-cognition`, `flux-agent`, `flux-flow`, `flux-sdk` | available |

`flux-core` is the root every other crate depends on, so **the publish cannot proceed under the current
names.** You can verify the squat yourself:

```sh
cargo package -p flux-lang --allow-dirty   # fails: flux-core ^0.2.4 resolves to the foreign 0.5.2
curl -A 'you (you@example.com)' https://crates.io/api/v1/crates/flux-core | jq .crate.newest_version
```

### Options (a decision for the maintainer)

1. **Vanity-prefix the whole namespace** (recommended for a clean brand): publish as
   `codewandler-flux-*` while keeping the Rust import paths unchanged. For each crate set the *package*
   name but pin the *lib* name:
   ```toml
   [package]
   name = "codewandler-flux-core"   # the crates.io name
   [lib]
   name = "flux_core"               # the import path stays `flux_core`
   ```
   and have dependents reference it via the `package` key:
   ```toml
   # [workspace.dependencies]
   flux-core = { package = "codewandler-flux-core", version = "0.2.4", path = "crates/flux-core" }
   ```
   Re-run the §1 availability check against the prefixed names first.
2. **Rename only `flux-core`** to an available name (e.g. `flux-kernel`) using the same package/lib-name
   split, leaving the other 15 names as-is. Smaller diff, but a mixed naming scheme.
3. **Publish to a private/alternate registry** (no crates.io name contention). Set `[registries]` and
   `--registry`.

Until one of these is chosen and applied, treat the steps below as a dry template.

## 2. The closure & topological publish order

16 crates. Publish in this order (each crate's dependencies precede it). This order is derived from the
crate dependency graph and verified against `cargo tree -p flux-sdk -e normal`:

```
1.  flux-core          ← root (no flux-* deps)
2.  flux-spec
3.  flux-policy
4.  flux-secret
5.  flux-evidence
6.  flux-skill
7.  flux-system        (→ core)
8.  flux-provider      (→ core)
9.  flux-session       (→ core)
10. flux-lang          (→ core, spec, policy, evidence)
11. flux-runtime       (→ core, spec, secret, system, policy, evidence)
12. flux-tools         (→ core, runtime, evidence, spec, system, policy)
13. flux-cognition     (→ core, spec, runtime, provider, lang, system)
14. flux-agent         (→ core, provider, runtime, session, spec, evidence, skill, system, tools)
15. flux-flow          (→ lang, core, spec, runtime, provider, session, agent, evidence, skill, tools, system)
16. flux-sdk           (→ core, provider, runtime, system, tools, session, agent, lang, flow, cognition)
```

Crates **not** in the closure (`flux-app`, `flux-tui`, `flux-server`, `flux-eval`, `flux-codegate`, the
provider backends, …) are **not** published by this runbook and stay path-only in
`[workspace.dependencies]`.

> **Provider backends.** `flux-sdk` is provider-agnostic, so `flux-anthropic` / `flux-openai` are *not*
> in the closure. A user embedding the SDK still needs a concrete provider — publishing those (and their
> own small closures: `flux-credentials`, …) is a sensible follow-on, scoped separately.

## 3. Version metadata (already applied)

Every closure crate carries `version = "0.2.4"` alongside `path` in `[workspace.dependencies]` (root
`Cargo.toml`) — cargo uses the path locally and the version in the *published* manifest. **On every
release, bump these in lockstep with `[workspace.package].version`** (a `cargo-release`/`cargo-dist`
config can automate this). Non-closure crates remain path-only.

## 4. Pre-flight (no registry writes)

```sh
# Per-crate packaging validation (build + metadata) — works offline against local path deps:
for c in flux-core flux-spec flux-policy flux-secret flux-evidence flux-skill \
         flux-system flux-provider flux-session flux-lang flux-runtime flux-tools \
         flux-cognition flux-agent flux-flow flux-sdk; do
  cargo package -p "$c" --allow-dirty || echo "FAILED: $c"
done
```

Note: a **full-graph `cargo publish --dry-run` is not possible before the deps are actually on
crates.io** — a downstream crate's dry-run verify build resolves its `flux-*` deps from the registry,
which won't have them yet. Only the **leaf** (`flux-core`, and the other dep-less crates) can be fully
dry-run today:

```sh
cargo publish --dry-run -p flux-core   # leaf: fully meaningful (no unpublished deps)
```

(With the current names this leaf dry-run *packages* fine but a real publish would be rejected — the
name is owned by someone else; see §1.)

## 5. The actual publish (needs your token — irreversible)

Once §1 is resolved and §4 is green:

```sh
export CARGO_REGISTRY_TOKEN=...           # from https://crates.io/settings/tokens

# Reserve/confirm ownership of each name BEFORE publishing (after a first publish you own it):
#   cargo owner --add <github-team-or-user> <crate>     # post-publish ownership

# Publish in the §2 order. crates.io needs a few seconds to index each crate before the next
# (dependent) crate can resolve it, hence the wait.
for c in flux-core flux-spec flux-policy flux-secret flux-evidence flux-skill \
         flux-system flux-provider flux-session flux-lang flux-runtime flux-tools \
         flux-cognition flux-agent flux-flow flux-sdk; do
  cargo publish -p "$c" || { echo "stopped at $c"; break; }
  sleep 20   # let the index update before the next crate resolves it
done
```

- **Irreversible.** A published `name@version` can never be reused — only yanked. Triple-check the
  version and that §4 is clean.
- **Tag the release** (`git tag v0.2.x && git push --tags`) so the published source is reproducible; the
  repo's `cargo-dist` pipeline already builds binaries on a tag.
- If a mid-sequence publish fails, fix and resume from the failed crate (earlier ones are already live).

## 6. Post-publish

- `cargo owner --add` the maintainers/team on each crate.
- Confirm docs.rs built each crate (the `documentation` field points at docs.rs).
- Smoke-test from a scratch project: `cargo add flux-sdk` then run the README quick-start.
