---
title: Troubleshooting
description: "Fast fixes for provider setup, state, command behavior, permissions, and common runtime failures."
sidebar_position: 4
---

# Troubleshooting

This page covers the common failures that block a first run or a local development loop. Each entry
starts with the command or config to check, then explains why flux behaves that way.

## flux says an API key is not set

You picked a provider whose credential isn't available. flux surfaces the exact variable it
looked for — e.g. `ANTHROPIC_API_KEY is not set`, `OPENAI_API_KEY is not set`,
`OPENROUTER_API_KEY is not set`, or for Bedrock `no AWS credentials: set
AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY, run aws sso login …`.

```bash
flux auth status          # shows every provider, what it needs, and where it resolved from
```

Set the environment variable, or run `flux auth login claude` / `flux auth login codex` for the
subscription paths. See [Providers and models](./agent/providers.md) for the full matrix.

## flux reports an auth error refreshing a stored login

A subscription login (`claude`, `codex`) stores a refresh token, and refresh tokens expire or get
revoked. When that happens mid-turn, flux surfaces the provider's actual reason and the fix in the
error itself — sign in again to mint a fresh token:

```bash
flux auth login codex     # or: flux auth login claude
```

Nothing else needs resetting; the stored credential is replaced in place.

## I want to try flux without credentials

Use the offline `mock` provider — it drives the adaptive loop and guarded execution with no network, returning
**canned** output (it writes `flux-mock.txt` and prints `Finished.`). It's a wiring smoke test, not a
real agent response:

```bash
flux run --yes -m mock "write a quick note"
```

Any flow that never reaches a model op also runs without credentials.

## Where does flux keep its state?

There is no single state directory for every surface. The default user-wide locations are:

| Path | What it is |
| --- | --- |
| `~/.flux/events.db` | append-only sessions, run traces, usage, and cross-session memory |
| `~/.flux/flow.db` | stored flow values, symbols, and suspensions |
| `~/.flux/credentials.toml` | plaintext provider and plugin tokens, protected with mode `0600` |
| `~/.flux/endpoints.toml` | imported weak endpoint references (credential locations, never values) |
| `~/.flux/flows/` | reusable flows and composite ops (`.flux` files) |
| `~/.flux/config.toml` | user-wide configuration defaults |
| `~/.flux/pricing.toml` | optional price overrides (see [Usage & cost](./agent/cost.md)) |
| `~/.flux/plugins/` | plugin descriptors, versioned binaries, and source-install cache |
| `~/.flux/connectors/` | installed connector manifests |

A project can also carry `.flux/config.toml`, `.flux/flows/`, `.flux/agents/`, `.flux/skills/`,
`.flux/commands/`, and `.flux/hooks/` in the exact directory where flux starts. Project configuration
and definitions are not copied into `~/.flux`, and flux does not walk upward to find a parent
repository.

`flux --store <dir> …` relocates that invocation's `events.db` and `flow.db`; it exports the same
choice as `FLUX_STORE_DIR` to child flux processes. It does not relocate credentials, endpoints,
plugins, project files, or the global store read by `flux usage`.

Do not delete an open SQLite database to troubleshoot it. The safest clean-room test is a new store:

```bash
flux --store ./tmp-flux-state run -m mock --yes "write a quick note"
```

If you intentionally reset persistent history, stop every flux process and back up the database
together with any `-wal` and `-shm` sidecars first. Removing `events.db` loses sessions and memory;
removing `flow.db` separately loses flow-engine state. See
[Storage & persistence](./reference/storage.md) for relocation, retention, and backend details.

## The server refuses to start

Binding to a non-loopback address without authentication is refused:

```text
refusing to serve on a non-loopback address (0.0.0.0:8787) without authentication — set
FLUX_SERVER_TOKEN to require `Authorization: Bearer <token>` (or configure
`[server] introspect_url` for per-request principal auth), or bind 127.0.0.1
```

The daemon auto-approves admitted tool calls within its configured ceilings, so an open listener with
effectful authority would be remote code execution. Either supply a shared secret, configure
principal auth, or bind loopback:

```bash
export FLUX_SERVER_TOKEN=$(openssl rand -hex 32)
flux app run --serve 0.0.0.0:8787 --yes
# …or bind loopback, which needs no token:
flux app run --serve 127.0.0.1:8787 --yes
```

Every route except `GET /health` and the A2A discovery card then requires
`Authorization: Bearer $FLUX_SERVER_TOKEN`.

## A native web operation refuses to reach a host

The SSRF guard rejects private, loopback, and link-local targets:
`refusing to fetch private/loopback/link-local address <ip> (<host>)`, or
`refusing to fetch internal host <host>`. This is deliberate — the guard resolves the hostname
to IPs and blocks the request if *any* resolved address is internal.

Grant the specific host to the native web family if you really need it:

```toml
# .flux/config.toml
[private_net]
web = ["localhost"]     # or `true` for any private host; covers http.request, web.fetch, browser.*
```

The retired `web_fetch = …` key is **not** ignored — `[private_net]` rejects unknown keys, so an old
config still carrying it refuses to load with `unknown field \`web_fetch\``. Migrate it to the
family-wide `web` key shown above.

See [Configuration](./reference/config.md) for the full grant shape (plugins are granted
separately under `[private_net.plugins]`).

## flux won't run a shell command

The generic `bash` op is opt-in — it is not surfaced unless the `shell` group is enabled, because
it necessarily runs `sh -c`. flux prefers dedicated argv-only ops (`read`, `write`, `grep`, the
`git_*`/`cargo_*` toolchains, …) which don't need a shell. To enable it:

```toml
# .flux/config.toml
enable_shell = true
```

…or set `FLUX_ENABLE_BASH=1`, or toggle it for a REPL session with `/shell`.

## A destructive step prompts even though I allow-listed the tool

Destructive operations (`rm -rf`, `git push --force`, …) re-fire the approval gate even under a
permissive `[permissions] allow` rule and inside an already-approved action batch. A destructive op
that was not visible in the approved batch prompts again at dispatch. This is intentional and
covered by tests; see [Safety & approvals](./agent/safety.md).

Passing `--yes` auto-approves every admitted step, including destructive ones, but does not widen a
policy, app, or agent ceiling. Use it only in trusted, non-interactive contexts (CI, the server
daemon).

## My context keeps getting compacted

Long sessions are summarized once they exceed a character budget
(`FLUX_COMPACT_CHARS`, default `48000`). Raise it, or disable compaction with `0`:

```bash
FLUX_COMPACT_CHARS=0 flux run "…"     # never compact (may hit the provider context limit)
```

## How do I resume a previous session?

```bash
flux run -c                 # continue the most recent session
flux sessions               # list past sessions
flux sessions --prune       # delete abandoned zero-message sessions
```

Inside the REPL, `/sessions` lists recent sessions and `/resume <id>` reattaches.

## A model spec is rejected

Use `-m <provider>/<model>`, e.g. `-m anthropic/claude-sonnet-4-6` or
`-m openrouter/anthropic/claude-sonnet-4.5`. The bare aliases `opus` / `sonnet` / `haiku` / `fable`
resolve to Anthropic; `claude`, `codex` and `aws` are bare aliases for their own providers. The
rejection message lists the accepted bare aliases, so trust it over this page if the two ever
disagree. The string after the provider is forwarded verbatim, so an unknown id usually surfaces as
a provider-side error. Routing acceptance is not a compatibility guarantee: the adaptive agent needs
a model and endpoint that reliably implement the provider's structured tool-call contract. A served
text-only model may route successfully and still be unsuitable for an agent turn.

## Plugin install fails verification

The install is fail-closed: the pack index is minisign-checked against the key embedded in flux,
and each archive's SHA-256 is checked against that index. A signature or checksum mismatch aborts
the install rather than proceeding. If you're building plugins from a source tree, use the
unverified local path instead:

```bash
(cd plugins && cargo build --release) && flux plugin install --dir
```

See [Using plugins](./plugins/using-plugins.md).

## sandbox unavailable: bubblewrap not found

You turned on `[sandbox]`/`--sandbox` on Linux but flux warned (or, under `require`, refused to
start) with `bubblewrap (bwrap) not found on PATH`. flux never falls back to an unconfined spawn
silently — `on` mode warns once and continues unconfined, `require` mode is a hard startup error.

Install bubblewrap with your distro's package manager (`apt install bubblewrap`, `dnf install
bubblewrap`, `pacman -S bubblewrap`, …), or point flux at a binary that isn't on `PATH`:

```bash
FLUX_BWRAP_BIN=/opt/bwrap/bin/bwrap flux --sandbox run "…"
```

See [OS process sandboxing](./security/os-sandbox.md) for what the sandbox confines once it's
active.

## sandbox auto-degrades: unprivileged user namespaces are refused (NamespacesDenied)

`bwrap` is installed but flux's preflight probe classifies it `NamespacesDenied` and — under `on`
mode — auto-degrades to unconfined with a warning naming the reason (under `require`, this is a
hard startup error instead). This means the kernel or a security policy is refusing the
unprivileged user-namespace creation bubblewrap needs, not that bubblewrap itself is broken.

This is the **expected** state in several common environments:

- **Docker's default seccomp profile** blocks `unshare`/`clone` with `CLONE_NEWUSER` — this is why
  the terminal-bench eval containers and most default `docker run` sandboxes land here.
- **Hardened kernels / Debian ≤ 11** ship `kernel.unprivileged_userns_clone=0` by default; flip it
  with `sysctl -w kernel.unprivileged_userns_clone=1` if you control the host.
- **Ubuntu 23.10+'s AppArmor userns restriction** requires either an AppArmor profile permitting
  unprivileged user namespaces or `sysctl -w kernel.apparmor_restrict_unprivileged_userns=0`.

If you need confinement rather than a warned auto-degrade in one of these environments, either fix
the underlying policy (add `--privileged`/the right `--security-opt` to the container runtime, or
flip the sysctl) or accept that `[sandbox] require = true` will refuse to start there. See
[OS process sandboxing](./security/os-sandbox.md#posture-matrix) for the full off/on/require ×
available/degraded matrix.

## DNS fails only inside the sandbox

A sandboxed process can't resolve hostnames (`curl`, `cargo fetch`, `git clone` fail with name
resolution errors) while the same command works unsandboxed, and `[sandbox] network` is on. The
sandbox replaces `/run` with a fresh tmpfs to hide host sockets like `docker.sock`, which also
hides the resolver socket/config that most Linux distros keep under `/run`. flux re-exposes the
common ones read-only when the network is on — systemd-resolved (`/run/systemd/resolve`),
`resolvconf` (`/run/resolvconf`), and NetworkManager (`/run/NetworkManager`). If your distro keeps
its resolver state somewhere else, adding that path (or the directory `/etc/resolv.conf` symlinks
into) to `[sandbox] writable` is *not* the fix — instead file it as a gap; the built-in re-bind
list is what needs extending. As a workaround, a static `/etc/resolv.conf` (not a symlink into
`/run`) resolves fine because the whole filesystem is visible read-only.

## Related docs

- [Providers and models](./agent/providers.md) — credential sources and model routing.
- [Configuration](./reference/config.md) — permissions, private-network grants, and overrides.
- [Safety and approvals](./agent/safety.md) — approval behavior and destructive-operation checks.
- [OS process sandboxing](./security/os-sandbox.md) — the `[sandbox]` config and posture matrix.
