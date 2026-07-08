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

## I want to try flux without credentials

Use the offline `mock` provider — it drives the full plan/execute pipeline with no network, returning
**canned** output (it writes `flux-mock.txt` and prints `Finished.`). It's a wiring smoke test, not a
real agent response:

```bash
flux run --yes -m mock "write a quick note"
```

Any flow that never reaches a model op also runs without credentials.

## Where does flux keep its state?

Everything lives under `~/.flux/`:

| Path | What it is |
| --- | --- |
| `~/.flux/events.db` | the append-only event log (sessions, run traces, usage) |
| `~/.flux/flow.db` | stored flow values, symbols, suspensions |
| `~/.flux/config.toml` | user-wide config (a project `.flux/config.toml` overrides it) |
| `~/.flux/pricing.toml` | optional price overrides (see [Usage & cost](./agent/cost.md)) |
| `~/.flux/plugins/` | installed plugin manifests |

Deleting `events.db` discards history; it does not corrupt anything. See
[Storage & persistence](./reference/storage.md) for the backend details.

## The server refuses to start

Binding to a non-loopback address without a token is refused:
`refusing unauthenticated non-loopback bind on <addr>; set FLUX_SERVER_TOKEN or bind …`. The
daemon auto-approves tools, so an open listener would be remote code execution.

```bash
export FLUX_SERVER_TOKEN=$(openssl rand -hex 32)
flux app run --serve 0.0.0.0:8787 --yes
# …or bind loopback, which needs no token:
flux app run --serve 127.0.0.1:8787 --yes
```

Every route except `GET /health` and the A2A discovery card then requires
`Authorization: Bearer $FLUX_SERVER_TOKEN`.

## web_fetch refuses to reach a host

The SSRF guard rejects private, loopback, and link-local targets:
`refusing to fetch private/loopback/link-local address <ip> (<host>)`, or
`refusing to fetch internal host <host>`. This is deliberate — the guard resolves the hostname
to IPs and blocks the request if *any* resolved address is internal.

Grant the specific host in config if you really need it:

```toml
# .flux/config.toml
[private_net]
web_fetch = ["localhost"]     # or `true` for any private host, web_fetch only
```

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
permissive `[permissions] allow` rule, and even inside an already-approved plan scope — a
destructive op that wasn't visible in the approved plan prompts again at dispatch. This is
intentional and covered by tests; see [Safety & approvals](./agent/safety.md).

Passing `--yes` auto-approves everything, including destructive steps, so use it only in trusted,
non-interactive contexts (CI, the server daemon).

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
`-m openrouter/anthropic/claude-sonnet-4.5`. The bare aliases `opus` / `sonnet` / `haiku` resolve
to Anthropic. The string after the provider is forwarded verbatim, so any model that provider
serves works — a typo surfaces as a provider-side error, not a flux one.

## Plugin install fails verification

The install is fail-closed: the pack index is minisign-checked against the key embedded in flux,
and each archive's sha256 is checked against that index. A signature or checksum mismatch aborts
the install rather than proceeding. If you're building plugins from a source tree, use the
unverified local path instead:

```bash
(cd plugins && cargo build --release) && flux plugin install --dir
```

See [Using plugins](./plugins/using-plugins.md).

## Related docs

- [Providers and models](./agent/providers.md) — credential sources and model routing.
- [Configuration](./reference/config.md) — permissions, private-network grants, and overrides.
- [Safety and approvals](./agent/safety.md) — approval behavior and destructive-operation checks.
