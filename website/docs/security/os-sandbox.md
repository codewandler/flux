---
title: OS process sandboxing
description: How flux's opt-in OS-level sandbox (bubblewrap on Linux, Seatbelt on macOS) confines shell and plugin subprocesses underneath the capability sandbox.
---

# OS process sandboxing

flux can wrap shell/exec ops and, above all, **stdio plugin subprocesses** in a platform sandbox:
bubblewrap on Linux, Seatbelt (`sandbox-exec`) on macOS. A small set of trusted host/browser
launches is explicitly exempt and documented below.
This is defense-in-depth **underneath** the safety envelope, applied at flux's single process
choke point. It is opt-in and off by default.

## A second, lower boundary — not a replacement for the first

[Safety & approvals](../agent/safety.md) and the [plugin capability sandbox](./plugin-sandbox.md)
remain the primary boundary and are unaffected by this page: they govern what a model may *ask*
flux to do, and what a plugin may *ask the host* to do, through the declared authorization/
capability protocol. That model doesn't change.

What OS process sandboxing adds is underneath it: a plugin binary — or any of the code paths it
links against — can bypass the capability-callback protocol entirely with a direct syscall,
because native code is trusted, not interpreted. The capability sandbox has no answer to that; it
governs the callback protocol, not the raw process. OS-level sandboxing does: it confines what
those raw syscalls can actually reach on disk and on the network, regardless of whether the
process honors the callback protocol at all.

| Layer | Governs | Boundary |
|---|---|---|
| Capability sandbox | What a plugin may *ask* the host to do | The declared manifest + capability-callback protocol |
| OS process sandbox | What a spawned process's *raw syscalls* can reach | The kernel — bubblewrap namespaces / Seatbelt profile |

## What it confines, per platform

| Platform | Backend | Status |
|---|---|---|
| Linux | bubblewrap (`bwrap`) | **Verified.** Golden-argv tests plus live smokes against a real `bwrap` binary: writes outside the workspace fail, network-off blocks a loopback connection, plugin stdio round-trips unchanged, and killing a sandboxed process leaves no orphan. |
| macOS | Seatbelt (`sandbox-exec`) | **Code-complete, pending hardware verification.** Golden-profile tests are hermetic and run on every platform (including Linux CI); the discovery/preflight code cross-checks clean against a real `x86_64-apple-darwin` target. No macOS CI exists yet, so exec-in-place semantics, a real `cargo build` under the profile, and TMPDIR canonicalization have not been exercised against real hardware. Treat it as implemented, not yet field-proven. |
| Windows | — | **No real backend yet.** The sandbox posture (config, CLI flags, env, warnings, fail-closed `require`) is fully wired and *is* the shipped v1 behavior on Windows: `on` mode warns once and runs unconfined; `require` mode refuses to start. A real backend (AppContainer / Job Objects) is a follow-up. |

Policy semantics are the same wherever a backend is active:

- **Read**: the whole filesystem is visible, read-only — toolchains, `/etc`, TLS certs, and
  locales just work without explicit binds.
- **Write**: confined to the workspace root, any `@named` write-capable root, validated linked-Git-
  worktree administrative/common directories, `/tmp` and
  `$TMPDIR`, the toolchain caches (`CARGO_HOME`/`~/.cargo`, `RUSTUP_HOME`/`~/.rustup` — needed
  because `cargo`/`rustup` invocations would otherwise break under confinement), and any
  `[sandbox] writable` extras. Missing configured paths are created as directories before launch and use a
  required bind; a writable `/` is rejected unless filesystem confinement was explicitly lifted.
- **Network**: on or off for the whole process, via a network namespace (Linux) or a Seatbelt
  `(deny network*)` clause (macOS). Default is unrestricted (open); narrowing is opt-in.
- `--allow-all-paths` lifts filesystem confinement from the sandbox too (with a warning); network
  policy still applies on top of it. On Linux the root bind is ordered before the protected
  `/dev`, `/proc`, and `/run` mounts, so those mounts remain in force.
- **Host IPC** (Linux): `/run` is a fresh tmpfs. When networking is open, flux restores only known
  resolver files for DNS; host D-Bus, NetworkManager, and systemd-resolved sockets stay masked.

## Turning it on

Off by default. Enable it with a flag, a config table, or the environment; when more than one
source has an opinion the **strictest posture wins** (`require` beats `on` beats off), so
`--sandbox` on top of `[sandbox] require = true` stays `require`. The lone exception is the kill
switch — `--no-sandbox` or `FLUX_SANDBOX=off` — which forces sandboxing off outright. An
unrecognized or empty `FLUX_SANDBOX` value never downgrades a configured posture, and a config file
that fails to parse is a hard startup error rather than silently dropping a configured `require`.

```bash
flux --sandbox run "…"       # turn on for this invocation
flux --no-sandbox run "…"    # force off — the kill switch, wins over env and config
```

```toml
# .flux/config.toml
[sandbox]
enabled = true      # turn on OS sandboxing for spawned processes
require = false     # fail closed instead of warn-and-continue when no backend is usable (implies enabled)
network = true       # omit for the unrestricted default; false closes the sandbox's network namespace/profile
writable = ["../shared-output"]   # extra writable paths beyond the workspace root and toolchain caches
```

| Config key | CLI flag | Env var | Meaning |
|---|---|---|---|
| `[sandbox] enabled` | `--sandbox` / `--no-sandbox` | `FLUX_SANDBOX=on\|off\|require` | Turn sandboxing on for spawned processes. |
| `[sandbox] require` | — | `FLUX_SANDBOX=require` | Fail closed (refuse to spawn) instead of warning when no backend is usable. |
| `[sandbox] network` | — | `FLUX_SANDBOX_NET` (truthy = open) | Whether sandboxed processes may reach the network. Unset/absent means open. |
| `[sandbox] writable` | — | `FLUX_SANDBOX_WRITABLE` (`:`-separated) | Extra writable paths, beyond the workspace/named/Git roots/tmp/toolchain caches. Missing paths are created as directories; `/` is rejected. |
| — | — | `FLUX_BWRAP_BIN` | Override which `bwrap` binary is used (Linux). Always resolved to an absolute path. |
| — | — | `FLUX_SANDBOX_EXEC_BIN` | Override which `sandbox-exec` binary is used (macOS). Always resolved to an absolute path. |
| — | — | `FLUX_SANDBOXED` | Set by flux itself on a genuinely-sandboxed child; a nested flux invocation sees this and skips re-wrapping (the outer namespaces already confine it). Not meant to be set by hand. |

`FLUX_SANDBOX`/`FLUX_SANDBOX_NET`/`FLUX_SANDBOX_WRITABLE` are exported by the CLI so a child flux
invocation (`app run`, an eval child host, `plugin call`) inherits the parent's posture without
re-parsing flags. Config merge is security-directional: a project's `[sandbox]` can only tighten a
user's, never loosen it — `enabled`/`require` are OR'd, `network` is strictest-wins, and
`writable` concatenates (the same documented widening as `[workspace] add_dirs`). See
[Configuration](../reference/config.md) for the full merge model.

## Posture matrix

| Mode | Backend available | Backend degraded / missing |
|---|---|---|
| `off` (default) | No confinement attempted; no backend probe runs at all. | Same — the common case pays nothing. |
| `on` | Confined. | **Auto-degrades**: one styled startup warning naming the reason, then runs unconfined for the rest of the session. |
| `require` | Confined. | **Fails closed**: a hard startup error, and a per-spawn backstop if something slips past startup — flux refuses any confinement-required spawn rather than degrade it. The explicit trusted host/browser exemptions below remain exempt by design. |

"Degraded" is not hypothetical: it is the expected state inside default-seccomp Docker, Debian ≤11
without the userns sysctl flipped, and Ubuntu 23.10+'s AppArmor userns restriction — all of which
refuse unprivileged user-namespace creation, which bubblewrap needs. flux's preflight probe
classifies this precisely (`NamespacesDenied`, distinct from "genuinely broken") so `on` mode
degrades gracefully in exactly these environments instead of treating every real spawn as a fresh
failure.

The probe itself uses the same guarded process launcher as product subprocesses: safe environment,
dedicated process group, bounded stderr, deadline, and descendant cleanup. Its inner command is
resolved to an absolute path from the caller's PATH before environment scrubbing, so non-FHS
systems such as NixOS and Guix are supported.

## Explicit host and browser exemptions

One spawn mode is deliberately **exempt** from sandboxing in v1: `spawn_debug_pipe`, the channel
flux uses to talk to a headless Chrome instance over the DevTools protocol. Chrome ships its own
content sandbox, and that sandbox needs a nested user namespace to set up. Forcing Chrome to run
with `--no-sandbox` so it fits inside bubblewrap's own namespace would trade a strong,
purpose-built content sandbox for a much weaker outer one — a net security loss, not a gain.
Browser confinement instead stays as it was before this epic: an env-cleared spawn plus CDP egress
interception (the SSRF guard applied to everything the browser fetches). Sandboxing the browser
process itself is a candidate follow-up, not solved here.

One other spawn is deliberately exempt: the **terminal-bench eval harness**. Its host-side
`cargo build` and `tb` runner drive Docker directly, and the sandbox's mandatory `/run` masking
would hide `/run/docker.sock` and break them — and the eval's task *container* is already the
isolation boundary. Those spawns run unsandboxed on purpose (marked as such in the code); the agent
being evaluated is confined by the container, not by this sandbox.

The **local eval child flux host** is also launched outside its child sandbox. That process must
call Anthropic, OpenAI, OpenRouter, or Ollama itself, so putting it in a `network = false` namespace
would prevent the provider request. The harness passes the resolved sandbox variables into the
host instead: provider traffic stays outside the child namespace, while the host's shell and plugin
descendants are confined at their own guarded spawn boundary. This exemption does not expose a
model-selected executable; it applies only to the harness-selected flux host binary.

## What v1 does not defend against

Stated plainly, so the guarantee isn't overclaimed:

- **Secret reads anywhere on the filesystem.** v1's read policy is "whole filesystem, visible,
  read-only" — `~/.ssh` and every other secret-bearing path stay readable to a sandboxed process.
- **Exfiltration while the network is on** (the default). The sandbox's network policy is
  whole-namespace on/off, not a destination allow-list.
- **Shared-`/tmp` interference.** The host's real `/tmp` is bind-mounted in, not replaced by a
  private tmpfs, so processes still share it the way they do unsandboxed.
- **cargo/rustup cache poisoning.** The toolchain caches are writable inside the sandbox (by
  design — otherwise every `cargo`/`rustup` invocation would break under confinement), so a
  malicious build script can still write into them.
- **Anything on Windows.** No real backend exists yet; `on` warns and runs unconfined, `require`
  fails closed, and that fail-closed refusal is the only protection Windows v1 offers.

None of these are silently accepted: they're documented trade-offs from the design (toolchain
writes need the cache to be writable; per-spawn network variance, secret-path masking, and seccomp
are deliberately deferred, not dead-ended), and each is a named candidate for a follow-up
hardening pass rather than an oversight.

## Related docs

- [Plugin capability sandbox](./plugin-sandbox.md) — what a plugin's code may *ask* the host to do.
- [Safety & approvals](../agent/safety.md) — the envelope every model-emitted operation traverses.
- [Configuration](../reference/config.md) — the full `[sandbox]` reference and merge model.
- [Troubleshooting](../troubleshooting.md) — bubblewrap-missing and `NamespacesDenied` fixes.
