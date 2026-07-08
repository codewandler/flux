---
title: Configuration
description: "Permissioning, environment precedence, and runtime settings that shape approval behavior and policy grants."
---

# Configuration

flux works with no project configuration, then lets you add policy, provider, and network settings as
the workspace needs them. Project configuration lives in `.flux/config.toml`; user defaults live in
`~/.flux/config.toml`.

**Precedence:** CLI flags > project `.flux/config.toml` > user `~/.flux/config.toml` > built-in
defaults. "Always-allow" choices you make at an approval prompt are saved back to the project file
automatically.

```toml
model = "sonnet"                 # default model (-m overrides; e.g. "anthropic/claude-sonnet-4-6")

[permissions]                    # deny wins, then allow, otherwise prompt
allow = ["read", "glob", "grep", "search", "Bash(git:*)"]
deny  = ["Bash(rm:*)"]

[private_net]                    # optional private/loopback egress grants (SSRF guard is on by default)
web_fetch = ["localhost"]        # or `true` for any private host — applies to web_fetch only

[private_net.plugins]            # per plugin (by manifest name), intersected with its declared hosts
prometheus = ["prometheus.local"]

[[policy.grants]]                # optional fine-grained authorization grants
subjects  = [{ kind = "user", id = "*" }]
resources = [{ kind = "path", path = "src/**" }]
actions   = ["workspace.write"]
```

## Permissions

`[permissions]` is the ergonomic layer over the authorization policy: `deny` is evaluated first, then
`allow`, otherwise flux prompts. Entries are tool names (`read`, `glob`, `search`) or scoped command
rules like `Bash(git:*)` / `Bash(rm:*)`. Reads are pre-allowed by default. Permission rules can never
widen past the policy floor, and destructive operations always re-fire the approval gate regardless
(an interactive run prompts for them; `--yes` auto-approves them along with everything else) — see
[Safety & approvals](../agent/safety.md).

## Private-network egress

The network guard refuses private, loopback, and link-local addresses by default (SSRF defense).
`[private_net]` grants scoped exceptions: `web_fetch` opts the fetch tool into named private hosts (or
`true` for any), and `[private_net.plugins]` grants a plugin access to specific hosts — always
intersected with the hosts the plugin declares in its manifest, so nothing undeclared is reachable.

To reach a private endpoint **once** without editing config — for a quick test, or a one-off
`flux plugin call` against internal infrastructure — pass the global `--allow-private-net` flag:

```bash
flux --allow-private-net plugin call gitlab gitlab.ci.job_token.allowlist.list '{"project":"group/app"}'
```

It is the ephemeral equivalent of a `[private_net.plugins]` grant for this invocation only (nothing
is persisted) and is audited like any config grant. Plugins still only reach the private hosts their
manifest declares; `web_fetch`, which has no manifest safeguard, is opened to **all** private ranges
(including cloud-metadata `169.254.169.254`) for the run — so prefer a scoped `[private_net.plugins]`
grant for anything recurring.

## Environment overrides

A few environment variables tune behavior without editing config: `FLUX_VERBOSE=1` (un-cap tool
output, same as `-v`), `FLUX_SHOW_LOOP=1` (reveal the [agent loop](../agent/agent-loop.md)),
`OLLAMA_HOST` (point at a non-default Ollama), and the provider API-key variables listed under
[Providers and models](../agent/providers.md).

## Related docs

- [Safety & approvals](../agent/safety.md) — how permissions and policy interact at dispatch time.
- [Credentials and secrets](../security/credentials.md) — provider tokens and plugin OAuth storage.
- [Private plugin networking](../security/plugin-sandbox.md#cross-plugin-and-private-network-grants) — manifest and config grants together.
