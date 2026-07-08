---
title: Using plugins
---

# Using plugins

A flux plugin is a subprocess binary (any language) that speaks a framed NDJSON protocol.
Each operation it declares is projected as a **policy-gated tool** — the same authorization,
approval, and guarded-IO chain as flux's built-in tools decides whether the agent may call it.

Plugins do **no privileged IO of their own**. Every side effect — HTTP, a subprocess, a socket, a
secret read — is a capability callback the host executes on the plugin's behalf, and the plugin
process is launched with a **cleared environment**: it gets only what its manifest declares.
Undeclared capabilities, hosts, secrets, and programs are denied by default.

## Install from the signed pack

The integration plugins ship separately from flux as the signed **plugin pack**
(`plugins-v*` releases). No source tree needed:

```bash
flux plugin install gitlab slack     # newest pack release
flux plugin install gitlab@0.1.0     # exact version
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
| Dev & project tracking | `gitlab`, `jira`, `confluence` |
| Infra & observability | `kubernetes`, `docker`, `aws`, `prometheus`, `loki`, `grafana`, `alertmanager`, `opsgenie` |
| Messaging | `slack` |
| Data | `sql` (PostgreSQL read-only query + introspection) |
| Telephony | `asterisk` (AMI), `homer` (SIP search / QoS / PCAP) |

Run `flux plugin skill` for the live per-plugin operation reference generated from the manifests
of what you actually have installed.

## Everyday commands

```bash
flux plugin ls                          # list installed plugins (the terse default)
flux plugin status [<name>]             # liveness + declared surface; omit the name for all
flux plugin call <name> <op> [json]     # invoke one op directly, no agent (alias: run)
flux plugin pin <name> <version>        # verified version switch (signed-index + checksum path)
flux plugin rollback <name>             # flip back to the previous version — offline, instant
flux plugin uninstall <name>            # remove the descriptor; --purge also deletes its store
```

Notes:

- `call` merges repeatable `--arg key=value` flags (coerced to the op's input schema) over the JSON
  input; `--dry-run` validates and prints the coerced input without spawning the plugin.
- `pin` records the binary's sha256 and re-checks it at every spawn — drift refuses to run.
- The versioned store keeps versions side by side, so `rollback` needs no network and a second
  `rollback` flips forward again.

```bash
flux plugin call websearch websearch.search '{"query":"warm transfer"}'
```

`flux run`, the REPL, and `flux app run` discover installed plugins at startup; the agent's grants
decide which of their ops it may call.

## Local / dev install

Building from a flux source checkout registers local binaries directly — unverified, with no
version or hash recorded. From the repo root:

```bash
(cd plugins && cargo build --release)   # → plugins/target/release/flux-plugin-<name>
flux plugin install --dir               # register every built flux-plugin-* binary
```

`--dir` defaults to `plugins/target/release`; pass a path to scan elsewhere. To register a single
arbitrary binary: `flux plugin add <name> <program> [args…]`.

## Granting network and secret access

Everything a plugin can touch is an explicit allow-list from its manifest: readable secret keys,
HTTP hosts, runnable programs, connection targets. Private/loopback network hosts additionally
require a grant in your config, per plugin (by manifest name):

```toml
[private_net.plugins]
prometheus = ["prometheus.local"]   # intersected with the plugin's declared private_hosts
```

The grant is **intersected** with what the plugin itself declares — you cannot grant a host the
manifest never named, and without a grant the private network is unreachable (deny-by-default).
See [Configuration](../reference/config.md).

A plugin that talks to an OAuth-protected API is logged in with `flux auth login <name>` — flux runs
the OAuth flow host-side and the plugin never sees the token. For the whole capability model,
references-only IO, and the manifest fields behind these grants, see
[Plugin capability sandbox](../security/plugin-sandbox.md); for the login flow and token storage, see
[Credentials & secrets](../security/credentials.md).

## Trust model

The capability gates above are enforced on the **host** side; the plugin binary itself is trusted,
pinned code — not OS-sandboxed. Review installed plugins the way you review dependencies. The
signed pack, sha256 pinning, and spawn-time hash re-check tell you *which* code runs; the manifest
gates and env-cleared spawn bound what that code can reach through flux.

Both halves are documented in depth under Security: [Plugin trust & signing](../security/plugin-trust.md)
for *which* code runs, and [Plugin capability sandbox](../security/plugin-sandbox.md) for what it can
reach.

## Writing your own

See [Plugin authoring](./authoring.md).
