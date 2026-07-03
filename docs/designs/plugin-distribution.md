# Design: plugin pack distribution (release channel + verified install)

**Status:** proposed (scoped by [D-21](../stories/D-21-plugin-distribution.md)) · **Pillar:** Core
(platform) · **Layer:** CI/release plumbing + L4 (`flux-plugin`) pack store + L6 (`flux-cli`) install
UX · **Owner:** Timo · **Stories:**
[D-46](../stories/D-46-plugin-pack-release-pipeline.md) ·
[D-47](../stories/D-47-remote-plugin-install.md) ·
[D-48](../stories/D-48-enforceable-pin-rollback.md) ·
[D-49](../stories/D-49-plugin-naming-docs-pass.md)

## Why

A flux user who did **not** clone the repo has no way to obtain the integration plugin pack. The only
path today is `cd plugins && cargo build --release && flux plugin install` — source tree + Rust
toolchain required. Anyone who installed flux the documented way (the release installers, or
`cargo install --git … flux-cli`) gets a `flux` that can *manage* plugins (`flux plugin
add/ls/pin/rollback/call/install/uninstall/status/skill`) but has nothing to install.

The facts the design builds on:

- **The pack is 17 thin binaries.** `plugins/` is a nested cargo workspace (deliberately excluded
  from the root gate via `Cargo.toml` `exclude = ["plugins"]`), one `[[bin]] flux-plugin-<name>` per
  integration (alertmanager, asterisk, aws, confluence, docker, gitlab, grafana, homer, huggingface,
  jira, kubernetes, loki, opsgenie, prometheus, slack, sql, websearch) plus the `host-kit` guest SDK
  library. Because the host does **all** privileged IO (no vendor SDKs, no TLS stacks in plugins),
  release binaries are 1.3–4 MB each, ~28 MB uncompressed for the whole pack on x86_64-linux, and the
  plugin sources contain **zero** `cfg(unix)`/`cfg(windows)` code — they are stdio loops over
  `flux.plugin.v1` (`crates/flux-plugin/src/lib.rs:28`).
- **Versioning is lockstep.** All pack crates share `workspace.package.version` (currently `0.1.0`),
  and every plugin reports its version in its manifest (`PluginManifest.version`, populated by
  host-kit's `ManifestBuilder`).
- **The core release channel is cargo-dist** (v0.32.0, `dist-workspace.toml`): five targets
  (`aarch64-apple-darwin`, `aarch64-unknown-linux-gnu`, `x86_64-apple-darwin`,
  `x86_64-unknown-linux-gnu`, `x86_64-pc-windows-msvc`), shell/powershell installers, sha256
  checksums, GitHub hosting. `release.yml` is dist-generated and triggers on any tag matching
  `'**[0-9]+.[0-9]+.[0-9]+*'`. The README's install one-liner depends on the repo-global
  `releases/latest/download/flux-cli-installer.sh` URL.
- **The lifecycle surface already exists** (D-19): descriptors at `~/.flux/plugins/<name>.toml`
  (`PluginDescriptor { program, args, pinned }`), `flux plugin install [dir]` scanning
  `flux-plugin-*` executables, `status` spawning and reading the manifest, name sanitization (D-35),
  env-cleared plugin spawn (D-22). But `pinned` is **advisory** — nothing enforces it at spawn, and
  nothing ties a descriptor to the bytes that were installed.

This design decides how a non-source user obtains the pack, and turns `pin`/`rollback` from advisory
labels into enforced supply-chain statements.

## Prior art (what comparable ecosystems teach)

- **terraform providers** (the gold standard): per-version × per-platform zips
  (`terraform-provider-<name>_<ver>_<os>_<arch>.zip`), one `…_SHA256SUMS` file signed **once** with a
  detached GPG signature covering all platforms transitively, a machine-readable versions/download
  protocol, and `.terraform.lock.hcl` pinning content hashes per platform. Weakness: the signing key
  is delivered by the same registry that serves the download, and key rotation (HashiCorp 2021) is
  painful with client-pinned keys.
- **krew (kubectl)**: a reviewed index of manifests, each carrying per-platform archive URI +
  **sha256**. The *index review*, not the download, is the integrity anchor — a swapped release asset
  fails the checksum. No signatures, but a meaningfully stronger baseline than helm/gh.
- **cargo-binstall**: no index; probes ~10 filename templates against the crate's declared repo, with
  a third-party build-farm fallback (QuickInstall) that silently shifts trust. Signing exists
  (minisign) but is opt-in and rarely adopted. Lesson: filename-guessing and implicit trust shifts
  are the failure modes; a manifest that *names* artifacts beats probing.
- **gh CLI extensions**: convention-only (`gh-<name>` repos, `<os>-<arch>` asset suffixes), **zero**
  verification at install — criticized precisely because an extension compromise is a credential
  compromise (the gh token is ambient). Great author UX, no trust story.
- **helm plugins** (the cautionary tale): `plugin.yaml` with *executable install hooks* that curl
  whatever they like; a string of CVEs from manifest fields reaching code paths. Lesson: the manifest
  must be **declarative data**, never executable.

Cross-cutting: anchor integrity in a signed/reviewed **index** fetched before any download; sign the
aggregate, not each artifact; keep a machine-readable manifest instead of naming conventions; record
hashes at install time and enforce them afterward.

## Decision: fetch-on-install from a signed, first-party **pack channel**

`flux plugin install <name>[@<version>]` downloads a prebuilt, per-plugin, per-target archive from a
dedicated **plugin-pack release series** in the flux repo, verifies it against a **signed index**,
and registers it. Nothing is bundled into the core release; there is no third-party marketplace yet —
but the index *is* the marketplace seed (a future marketplace is more index entries, not a new
mechanism).

Rejected alternatives:

- **Bundle the pack into the core release.** Size is *not* the blocker (~8–12 MB compressed per
  target) — coupling is: it drags the excluded `plugins/` workspace into the core dist build, welds
  the pack's cadence (parity work moves fast) to core tags, forces all-or-nothing installs, and gives
  no growth path to selective or third-party plugins. It would also re-litigate the deliberate
  root-workspace exclusion.
- **Full marketplace service** (search/publish/multi-publisher, the fluxplane `fluxplane-plugin`
  marketplace shape): there are zero third-party plugins today; a registry service is infrastructure
  without a user. The index schema below leaves the door open (per-plugin `version` fields, nothing
  hardwired to lockstep).
- **cargo-binstall-style source fallback in the CLI**: compiling requires the toolchain anyway; if
  you have one, the source path is two commands (below). No auto-compile machinery.

## The pack channel (artifacts + index)

**Release series.** Plugin pack releases are GitHub releases in the same repo, tagged
**`plugins-v<pack_version>`** (dash, not slash — keeps `releases/download/<tag>/<asset>` URLs
unambiguous). The pack version is the plugins workspace's lockstep `workspace.package.version`,
bumped per release, independent of core `vX.Y.Z` tags. Pack releases are created with
**`--latest=false`** so the core installer's `releases/latest/download/flux-cli-installer.sh` URL
never resolves to a pack release.

**Artifacts.** One archive per plugin per target —
`flux-plugin-<name>-<version>-<target>.tar.xz` (`.zip` on windows), containing the single executable
(`flux-plugin-<name>[.exe]`) — 85 assets across 5 targets, well within GitHub release limits, and the
shape that makes selective install, side-by-side versions, and future third-party entries all
uniform. Plus two channel-level assets:

- **`plugins-index.json`** — the machine-readable index (schema below).
- **`plugins-index.json.minisig`** — a minisign detached signature over the index.

**Index schema** (`schema: 1`):

```json
{
  "schema": 1,
  "pack_version": "0.2.0",
  "protocol": "flux.plugin.v1",
  "released_at": "2026-07-02T00:00:00Z",
  "plugins": {
    "gitlab": {
      "version": "0.2.0",
      "description": "GitLab projects, MRs, pipelines, …",
      "artifacts": {
        "x86_64-unknown-linux-gnu": {
          "asset": "flux-plugin-gitlab-0.2.0-x86_64-unknown-linux-gnu.tar.xz",
          "sha256": "…",
          "size": 1234567
        }
      }
    }
  }
}
```

Two hard rules, each a testable assertion (the endpoint-discovery style):

- **The index names assets, never URLs.** An `asset` value is a bare file name; the CLI constructs
  download URLs only from `(repo, tag, asset-name)` against `github.com/codewandler/flux`. A
  compromised or malicious index cannot redirect a download to another host (the binstall/helm
  failure mode).
- **The index is data, never behavior.** No hooks, no commands, no templates — resolution and
  verification are entirely host-side (the helm lesson).

**Resolution.** `install <name>` with no version lists the repo's releases, filters the `plugins-v`
prefix, and takes the highest semver; `install <name>@<version>` fetches tag `plugins-v<version>`
directly (no API call). Index names pass through the same `descriptor_path` sanitizer D-35
introduced before they touch the filesystem.

## Security model & supply chain

A plugin executes inside the host envelope — env-cleared spawn (D-22), deny-by-default manifest-gated
capabilities, no privileged IO of its own — but the binary is still native code on the user's
machine, runnable outside flux and granted real capabilities inside it. Distribution therefore gets
its own trust ladder; execution-side sandboxing is a complement, not a substitute (the gh-extensions
criticism, answered).

1. **Fixed origin + TLS.** Downloads come only from the flux repo's release URLs (rustls via the
   workspace `reqwest`); the index cannot point elsewhere (rule above).
2. **Signed index.** The CLI fetches `plugins-index.json` + `.minisig` and verifies with a minisign
   public key **embedded in the flux binary** (`minisign-verify` crate — pure Rust, no new native
   deps). One signature covers every artifact transitively via the per-artifact sha256 entries —
   terraform's sign-the-aggregate pattern. The secret key lives in CI (GitHub Actions secret);
   verification failure is fatal, no `--skip-signatures` escape hatch (the no-fallbacks rule).
3. **Checksum before executable.** The downloaded archive's sha256 is verified against the index
   entry *before* the binary is unpacked into place and made executable.
4. **Install-time recording.** The descriptor gains `version`, `sha256` (of the installed binary),
   and `source` (`plugins-v<version>`) fields; `PluginDescriptor` grows with serde defaults so
   existing hand-added descriptors stay valid — a dev descriptor (`add`, `install --dir`) simply
   carries no hash and is visibly labeled `unverified (local)` in `ls`/`status`.
5. **Enforcement afterward.** Spawn-time: a descriptor that carries a `sha256` is re-hashed before
   `PluginHost::spawn` (1–4 MB, sub-millisecond) — drift is a hard refusal naming the expected/actual
   hash. `flux plugin status` re-verifies hash + manifest-version agreement and reports drift loudly.
   This is what turns `pin` from advisory into enforced.

**Residual risk, stated honestly:** a compromised repo or CI can sign malicious artifacts — the key
custody is only as strong as the Actions secret. The upgrade path is GitHub artifact attestations
(sigstore provenance, `gh attestation verify` / sigstore-rs) layered *on top of* the minisign ladder;
deliberately deferred (weight of a sigstore verifier in flux-cli vs. an Actions-secret key) — noted
as future hardening, not Phase 1. Key rotation = ship a new flux release with the new embedded
pubkey (accepting the terraform lesson that rotation is the painful part; embedding N>1 accepted
keys keeps rotation graceful).

## Build & release plumbing

A new, hand-written **`release-plugins.yml`** — cargo-dist is *not* extended to the plugins
workspace (dist models one app per package; 17 lockstep packages would need per-package tags or
version-unified announcements colliding with core — wrong tool):

- **Trigger: `workflow_dispatch`** with a `version` input — *not* a tag push. This is deliberate:
  the dist-generated `release.yml` triggers on `'**[0-9]+.[0-9]+.[0-9]+*'`, which any semver-ish
  plugins tag also matches, and dist's plan job errors on a tag whose package prefix isn't dist-able.
  The workflow itself creates the `plugins-v<version>` tag + release via `GITHUB_TOKEN` (workflow-
  created refs don't re-trigger workflows, so no recursion; a hand-pushed `plugins-v*` tag would
  red-X the dist plan job — documented as "don't do that").
- **Matrix on native runners** — no cross-compilation: `ubuntu-latest` (x86_64-linux),
  `ubuntu-24.04-arm` (aarch64-linux), `macos-15-intel` (x86_64-darwin), `macos-latest` (aarch64-darwin),
  `windows-latest` (x86_64-windows). Each leg: `cargo build --release --workspace` inside `plugins/`,
  package per-plugin archives + per-artifact sha256, upload as workflow artifacts.
- **Assemble job**: collects all legs, generates `plugins-index.json` (versions read from the
  workspace, hashes from the legs), signs it with the CI minisign key, sanity-checks the pack
  (asset count = plugins × targets; every index entry resolves), and runs
  `gh release create plugins-v<version> --latest=false …`.
- **The core gate is untouched.** `ci.yml` keeps its separate `plugins` job (fmt/clippy/build/test on
  ubuntu); the root workspace exclusion stands; the core `release.yml` stays dist-generated and
  never sees the plugins workspace.

## Cross-platform

Same five targets as core — the pack has no platform-specific code and no vendor native deps (all
privileged IO is host capabilities), so all 17 plugins build everywhere the host does. Windows notes:
archives are `.zip`; binaries are `flux-plugin-<name>.exe`; the existing local-scan
`plugin_binaries_in` (`crates/flux-cli/src/main.rs:5452`) skips any file containing `.` and therefore
skips **every** binary on Windows — the remote path writes descriptors directly and doesn't care, but
D-47 fixes the scan (`.exe`-aware) so `--dir` works on Windows too.

**Source fallback** (kept, not built): `git clone … && cd plugins && cargo build --release &&
flux plugin install --dir plugins/target/release`. That is the whole fallback — documented, not
mechanized.

## CLI surface (clean cutover)

- **`flux plugin install <name>[@<version>] …`** — remote install (multiple names allowed;
  `--all` for the whole pack): resolve release → verify index signature → download → verify sha256 →
  unpack into the **versioned store** `~/.flux/plugins/bin/<name>/<version>/flux-plugin-<name>` →
  write descriptor (program, version, sha256, source). Idempotent re-install of a present version is
  a no-op with a note.
- **`flux plugin install --dir [path]`** — the current local-scan behavior, moved behind an explicit
  flag (default `plugins/target/release`). Bare `flux plugin install` with no names and no `--dir`
  becomes an error naming both modes — no guessing (clean cutover; the old implicit default only ever
  made sense inside the repo).
- **`flux plugin pin <name> <version>`** — ensures that version is in the store (downloads if
  absent), repoints the descriptor, records the hash, remembers the prior version in a `previous`
  field. **`rollback <name>`** — repoints to `previous`, offline and instant thanks to side-by-side
  versions. Both now *mean* something at spawn (enforcement ladder, step 5).
- **`flux plugin uninstall <name> [--purge]`** — descriptor removal as today; `--purge` also deletes
  the plugin's versioned store directory.
- **`flux plugin status`** — gains the verification column (hash ok / drift / unverified-local) on
  top of the D-19 liveness + surface report.
- **Compatibility check**: the CLI refuses an index whose `protocol` ≠ its own
  `flux_plugin::PROTOCOL` (`flux.plugin.v1`) with a "upgrade flux / pick an older pack" message.

Code placement (single-crate-with-modules rule): index schema, verification, and the versioned store
land as a `pack` module in **`crates/flux-plugin`** (L4) — it already owns descriptors and already
depends on `reqwest` + `sha2`; `minisign-verify` is the only new dep. `flux-cli` (L6) keeps only the
UX (arg parsing, progress, messages).

## Naming: the trio, disambiguated

Canonical vocabulary for all user-facing docs and help text (today the three are routinely conflated):

| Say | Meaning | Never call it |
| --- | --- | --- |
| **the plugin protocol crate** (`flux-plugin`) | `crates/flux-plugin` — the `flux.plugin.v1` host+guest library | "the plugin" |
| **the plugin pack** / **a plugin binary** (`flux-plugin-<name>`) | the `plugins/` workspace and its released `flux-plugin-<name>` binaries; release series `plugins-v<version>` | "flux-plugin" bare |
| **the plugin CLI** (`flux plugin …`, with the space) | the lifecycle surface in flux-cli | "flux-plugin" |

Rule of thumb: hyphen-no-suffix = the crate; hyphen-with-name = a pack binary; space = the CLI.
Release-page disambiguation is structural: core assets are `flux-cli-*` (the dist app name), pack
assets are `flux-plugin-<name>-*`, and the tag prefixes (`v` vs `plugins-v`) separate the series. A
future crates.io vanity prefix (`codewandler-flux-*`, see `crates/flux-sdk/PUBLISHING.md`) renames
the crate without touching the other two.

## Sequencing

```
D-46 (pack release pipeline: per-plugin artifacts + signed index + plugins-v release)   ← supply side
   │
D-47 (remote install: resolve → verify → versioned store → descriptor; --dir cutover)   ← demand side
   │
   ├── D-48 (enforced pin/rollback + spawn/status hash verification)
   └── D-49 (naming + docs truth pass across the trio)
```

D-46 and D-47 are strictly sequential (D-47 needs a real release to verify against, and its fixture
tests encode D-46's index schema). D-48 and D-49 are independent of each other after D-47.

## Non-goals

- A **third-party marketplace** (publisher identity, search, review pipeline) — the index schema is
  the seed; the service is not built until third-party plugins exist.
- **Auto-compile fallback** in the CLI (binstall-style) — the source path is two documented commands.
- **Auto-update / background updaters** — installs and upgrades are explicit CLI actions.
- **Per-plugin independent versioning** — lockstep pack releases for now; the index's per-plugin
  `version` field already permits divergence later without a schema break.
- **Extending cargo-dist to the plugins workspace**, publishing pack crates to crates.io, or
  Homebrew/npm installers for plugins.
- **Sigstore/attestation verification in flux-cli** — named as the hardening path, deferred.

## Reuse, don't reimplement

- The **descriptor store** + D-35 name sanitizer (`descriptor_path`) — index-supplied names go
  through the same single guard; the D-19 `status` report is the verification surface.
- **`flux-plugin`'s existing deps** (`reqwest` rustls, `sha2`) — no new HTTP/hash stack; only
  `minisign-verify` is added.
- **cargo-dist's shapes, not its machinery**: per-target archives, sha256 sidecars, a
  machine-readable per-release manifest — mimicked for the pack channel while the core channel stays
  dist-generated and untouched.
- **D-22's env-cleared guarded spawn** as the execution-side complement to distribution trust.
- **`scripts/smoke-plugins.sh`** as the post-release validation hook (env-gated, skips keyless
  integrations) — optional release-workflow step, not a gate.
- Prior-art shapes: terraform's signed `SHA256SUMS` + lock-file hashes; krew's hash-in-index;
  binstall's minisign choice (as the *default*, not opt-in); gh's per-platform asset naming.
