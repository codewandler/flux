---
title: Using plugins
description: "Installing and running trusted plugins, including signatures, capability grants, and host-enforced safety."
---

# Using plugins

Plugins are trusted subprocess binaries that extend flux with new operations. Each declared
operation is projected as a policy-gated tool, so the same authorization, approval, and guarded-IO
chain that protects built-ins also protects plugin calls.

First-party plugins follow a host-callback contract: HTTP, subprocess, connection, filesystem, and
secret operations are requested from flux, which checks the manifest and performs the operation.
The process also starts with a cleared, minimal environment, so it does not inherit provider or host
secrets.

That capability contract is **not an OS sandbox**. A plugin is a trusted native executable and could
make direct system calls outside flux if it were malicious. Install plugins as dependencies you
trust; the manifest gates constrain what conforming plugin code can reach *through flux*.

## Install from the signed pack

The integration plugins ship separately from flux as the signed **plugin pack**
(`plugins-v*` releases). No source tree needed:

```bash
flux plugin install gitlab slack     # newest pack release
flux plugin install gitlab@0.1.1     # exact version
flux plugin install --all            # the whole pack
```

Every install is verified end-to-end, fail-closed (there is no bypass flag):

1. The release's `plugins-index.json` is **minisign-verified** against the public key embedded in flux.
2. Each archive's **sha256** is checked against that verified index before unpacking.
3. Binaries land in the versioned store `~/.flux/plugins/bin/<name>/<version>/`; re-installing a
   version already present is an idempotent no-op.

Bare `flux plugin install` (no names, no `--all`, no `--dir`) is an error — it never guesses.

## What's in the pack

| Category | Plugins |
|---|---|
| Search & knowledge | `websearch` (Tavily + DuckDuckGo fallback), `huggingface` (Hub catalog + router chat/embed) |
| Dev, project & support tracking | `gitlab`, `jira`, `confluence` |
| Infra & observability | `kubernetes`, `docker`, `aws`, `prometheus`, `loki`, `grafana`, `alertmanager`, `opsgenie` |
| Messaging | `slack` |
| Data | `sql` (PostgreSQL and MySQL/MariaDB read-only query + introspection) |
| Telephony | `asterisk` (AMI), `homer` (SIP search / QoS / PCAP) |
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
- `pin` records the binary's sha256 and re-checks it at every spawn — drift refuses to run.
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
restart flux so the grant is made again against the new manifest.

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

Building from a flux source checkout registers local binaries directly — unverified, with no
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

A plugin that talks to an OAuth-protected API is logged in with `flux auth login <name>` — flux runs
the OAuth flow host-side and the plugin never sees the token. A plain bearer token (the common case:
Slack, GitLab) resolves from the env vars the manifest declares, or store it once with
`flux auth set <name> <purpose>` and no env var is needed in any later session. For the whole
capability model, references-only IO, and the manifest fields behind these grants, see
[Plugin capability sandbox](../security/plugin-sandbox.md); for login and token storage, see
[Credentials & secrets](../security/credentials.md).

## Trust model

The capability gates above are enforced on the **host** side; the plugin binary itself is trusted,
pinned code — not OS-sandboxed by default. Review installed plugins the way you review
dependencies. The signed pack, sha256 pinning, and spawn-time hash re-check tell you *which* code
runs; the manifest gates and env-cleared spawn bound what conforming code can reach through flux.
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
- [Plugin capability sandbox](../security/plugin-sandbox.md) — what a plugin may reach through flux.
- [OS process sandboxing](../security/os-sandbox.md) — opt-in confinement of the raw plugin process.
