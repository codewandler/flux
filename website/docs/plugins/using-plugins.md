---
title: Using plugins
description: "Installing and running trusted Flux plugins, including source choices, signatures, capability grants, and host-enforced safety."
---

# Using plugins

:::note Current compatibility path
The signed first-party plugin pack is the supported way to use GitLab, Slack, Docker, Kubernetes,
SQL, observability, secret-store, and other official integrations **today**. It is not their
permanent source model.

The accepted direction moves every official vendor-specific integration to
**flux-connectors**, with an explicit runtime that Flux can execute locally or Flux Exchange can
host under tenant authority. Flux keeps generic guarded runtimes—including the stdio plugin
protocol where it is useful—rather than permanent vendor-specific crates. Existing plugins remain
available until connector parity, local and hosted conformance, and an explicit cutover have all
passed. See [Connector-native integrations](../direction/connector-native-integrations.md).
:::

Plugins are trusted subprocess binaries that extend Flux with new operations. Each declared
operation is projected as a policy-gated tool, so the same authorization, approval, and guarded-IO
chain that protects built-ins also protects plugin calls.

First-party plugins follow a host-callback contract: HTTP, subprocess, connection, filesystem, and
secret operations are requested from Flux, which checks the manifest and performs the operation.
The process also starts with a cleared, minimal environment, so it does not inherit provider or host
secrets.

That capability contract is **not an OS sandbox**. A plugin is a trusted native executable and could
make direct system calls outside Flux if it were malicious. Install plugins as dependencies you
trust; the manifest gates constrain what conforming plugin code can reach *through Flux*.

## Choose an install source

The install source determines what Flux can verify:

| Source | Command | Verification label | Trust boundary |
|---|---|---|---|
| Signed pack | `flux plugin install <name>` or `--all` | `verified` | A minisign-verified index selects an archive whose SHA-256 is checked before install; the installed binary's SHA-256 is then checked at every spawn. |
| Git source build | `flux plugin install --git <url> …` | `from-source (unverified)` | Flux shows the resolved commit and requires explicit consent before running `cargo build`; the descriptor records Git URL + commit, not a signed-pack hash. |
| Local binary | `flux plugin install --dir[=<path>]` or `flux plugin add …` | `unverified (local)` | Flux registers a binary you already built; no version or hash is recorded. |

These modes are mutually exclusive. A signature proves release provenance, while a source commit or
local path only tells you what you chose to trust.

## Install from the signed pack

The integration plugins ship separately from Flux as the signed **plugin pack**
(`plugins-v*` releases). No source tree needed:

```bash
flux plugin install gitlab slack     # newest pack release
flux plugin install --all            # the whole pack
```

To select a pack version explicitly, append the version published in the pack index, using
`<name>@<version>`.

Every install is verified end-to-end, fail-closed (there is no bypass flag):

1. The release's `plugins-index.json` is **minisign-verified** against the public key embedded in Flux.
2. Each archive's **SHA-256** is checked against that verified index before unpacking.
3. Binaries land in the versioned store `~/.flux/plugins/bin/<name>/<version>/`; re-installing a
   version already present is an idempotent no-op.

Bare `flux plugin install` (no names, no `--all`, no `--dir`, and no `--git`) is an error — it never
guesses.

## Install from Git source

Use `--git` for a third-party or privately hosted plugin that is not in the signed pack:

```bash
flux plugin install --git https://gitlab.example/team/flux-plugin-acme.git --tag v1.0.0
```

Choose at most one of `--tag`, `--rev`, or `--branch`. Use `--bin flux-plugin-<name>` when a
repository contains several matching binaries, and `--force` to rebuild a commit already installed.
Flux clones into `~/.flux/plugins/src/`, resolves the selected ref to a commit, builds with
`cargo build --release --locked`, and copies the result into the versioned plugin store.

:::danger A source install executes unverified code
Cargo builds can execute build scripts and procedural macros. Before building, Flux displays the Git
URL and resolved commit and asks for explicit confirmation, defaulting to **no**. For unattended
installation, `FLUX_ALLOW_SOURCE_BUILD=1` is the explicit consent signal; it is not a verification or
sandboxing switch.
:::

The descriptor records the Git URL and resolved commit, but no signed-pack SHA-256. Flux therefore
labels the install `from-source (unverified)` and does not make the signed pack's spawn-time
hash-integrity claim. Review the repository and pin `--rev` when you need the requested source ref
itself to be immutable. Reinstalling the same resolved commit is an idempotent no-op unless you pass
`--force`.

## What's in the compatibility pack

| Category | Plugins |
|---|---|
| Search & knowledge | `websearch` (Tavily + DuckDuckGo fallback), `huggingface` (Hub catalog + router chat/embed) |
| Dev, project & support tracking | `gitlab`, `jira`, `confluence` |
| Infra & observability | `kubernetes`, `docker`, `aws`, `prometheus`, `loki`, `grafana`, `alertmanager`, `opsgenie` |
| Messaging | `slack` |
| Data | `sql` (PostgreSQL and MySQL/MariaDB read-only query + introspection) |
| Telephony | `homer` (SIP search / QoS / PCAP) |
| Secret management | `vault` (KV-v2 administration), `onepassword` (Connect server) |

Run `flux plugin skill` for the live per-plugin operation reference generated from the manifests
of what you actually have installed.

## Everyday commands

```bash
flux plugin ls                          # list installed plugins (the terse default)
flux plugin status [<name>]             # liveness + declared surface; omit the name for all
flux plugin call <name> <op> [json]     # invoke one op directly, no agent (alias: run)
flux plugin refresh <name>              # re-fetch the manifest and re-project the op catalog
flux plugin pin <name> <version>        # verified version switch (signed-index + checksum path)
flux plugin rollback <name>             # flip back to the previous version — offline, instant
flux plugin uninstall <name>            # remove the descriptor; --purge also deletes its store
```

Notes:

- `call` merges repeatable `--arg key=value` flags (coerced to the op's input schema) over the JSON
  input. With `--dry-run`, the plugin process is still spawned once to read its manifest and schema,
  but the selected operation is never invoked and no operation-level network or write occurs.
- `pin` records the signed-pack binary's SHA-256 and re-checks it at every spawn — drift refuses to
  run.
- The versioned store keeps versions side by side, so `rollback` needs no network and a second
  `rollback` flips forward again.
- `refresh` is for plugins whose operation set depends on remote state. A plugin answers `manifest`
  over its live connection, not from a file, so one that fronts a remote deployment can advertise
  new operations once you authenticate a provider there (`flux auth login <name>`). `refresh`
  re-fetches the manifest and re-projects the catalog, printing which operations appeared and which
  were withdrawn.

### What a refresh may and may not change

A refresh is a **re-grant**, so it is bounded by the grant you already made. The operation set is
free to change — operations may appear and disappear, and their schemas and descriptions may change
with them. Two things may not:

- **The capabilities.** The grant made when the plugin loaded is the one that stays in force, in
  both directions. A refreshed manifest that asks for *more* — programs, secret keys, HTTP hosts,
  dial targets, file scopes, or any other host capability — is refused. One that asks for *less* is
  accepted but not adopted: giving up a capability in the manifest does not reduce what an operation
  must be authorized for, because the capability itself is still granted. The same applies to the
  plugin's declared endpoints, auth purposes and config keys.
- **The scope of an operation that keeps its name.** Policy rules, permission subjects and session
  grants all key on the operation name, so an operation cannot keep its name while dropping its risk
  tier, shedding a declared effect, or widening its per-operation `process` narrowing.

Either refusal leaves the catalog exactly as it was — a refresh never half-applies, and neither does
a dead subprocess or an unreadable manifest frame. To adopt a genuinely changed capability set,
restart Flux so the grant is made again against the new manifest.

An operation the plugin withdraws stops being callable. A call already running against it finishes
under the specification it was authorized with; withdrawal governs the next dispatch, not the one in
flight.

```bash
flux plugin call websearch websearch.search '{"query":"warm transfer"}'
```

`flux run`, the REPL, and `flux app run` discover installed plugins at startup; the agent's grants
still decide which operations may execute. For open-ended CLI turns, ungrouped plugin operations are
advertised on demand: name the integration in the request (for example, “list the open GitLab merge
requests” or “post this in Slack”) and Flux surfaces that integration's catalog for the rest of the
engine session. Unrelated installed plugins stay out of native model-stage catalogs. Only operations
from installed plugins that are actually loaded and wired can surface. Direct `flux plugin call`
and authored app/flow operation lists are unchanged.

## Local / dev install

Building from a Flux source checkout registers local binaries directly — unverified, with no
version or hash recorded. From the repo root:

```bash
(cd plugins && cargo build --release)   # → plugins/target/release/flux-plugin-<name>
flux plugin install --dir               # register every built flux-plugin-* binary
```

`--dir` defaults to `plugins/target/release`; attach a custom path with
`--dir=/path/to/binaries`. To register a single arbitrary binary, use
`flux plugin add <name> <program> [args…]`.

## Granting network and secret access

Every host capability a plugin can request is an explicit allow-list from its manifest: readable
secret keys, HTTP hosts, runnable programs, connection targets. Private/loopback network hosts
additionally require a grant in your config, per plugin (by manifest name):

```toml
[private_net.plugins]
prometheus = ["prometheus.local"]   # intersected with the plugin's declared private_hosts
```

The grant is **intersected** with what the plugin itself declares — you cannot grant a host the
manifest never named, and without a grant the private network is unreachable through host
capabilities. See [Configuration](../reference/config.md).

A plugin that talks to an OAuth-protected API is logged in with `flux auth login <name>` — Flux runs
the OAuth flow host-side and the plugin never sees the token. A plain bearer token (the common case:
Slack, GitLab) resolves from the env vars the manifest declares, or store it once with
`flux auth set <name> <purpose>` and no env var is needed in any later session. For the whole
capability model, references-only IO, and the manifest fields behind these grants, see
[Plugin capability sandbox](../security/plugin-sandbox.md); for login and token storage, see
[Credentials & secrets](../security/credentials.md).

Host-terminated connection handshakes are different: the SQL plugin's static endpoint path reads
its declared password environment variables directly and does not consult `flux auth set` storage.

## Trust model

The capability gates above are enforced on the **host** side; the plugin binary itself is trusted,
native code — not OS-sandboxed by default. Review installed plugins the way you review
dependencies. For verified pack installs, SHA-256 pinning and the spawn-time hash re-check tell you
*which* code runs; the manifest gates and env-cleared spawn bound what conforming code can reach
through Flux.
Opt-in [OS process sandboxing](../security/os-sandbox.md) (`[sandbox]`) additionally confines what
the raw plugin binary's syscalls can reach on disk and network.

Both halves are documented under Security: [Plugin trust & signing](../security/plugin-trust.md)
for *which* code runs, and [Plugin capability sandbox](../security/plugin-sandbox.md) for what host
capabilities it can request.

## Writing your own

See [Plugin authoring](./authoring.md).

## Related docs

- [Plugin authoring](./authoring.md) — write a plugin and manifest.
- [Plugin trust & signing](../security/plugin-trust.md) — signed index, hashes, and spawn-time checks.
- [Plugin capability sandbox](../security/plugin-sandbox.md) — what a plugin may reach through Flux.
- [OS process sandboxing](../security/os-sandbox.md) — opt-in confinement of the raw plugin process.
