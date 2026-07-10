# Design: OS process sandboxing — bubblewrap / Seatbelt at the spawn choke point

**Status:** proposed (2026-07-10) · **Pillar:** Core · **Layer:** L0 (flux-config) + L2 (flux-system) + L6 (flux-cli) · **Stories:** [D-134](../stories/D-134-sandbox-abstraction-config-threading.md) · [D-135](../stories/D-135-bubblewrap-backend.md) · [D-136](../stories/D-136-seatbelt-backend.md) · [D-137](../stories/D-137-sandbox-docs-truth-pass.md)

## Why

flux's safety envelope (authorization → approval → guarded IO) governs what the *model* may
request, but the OS processes it ultimately spawns — shell commands and, above all, **stdio
plugins** — run with the invoking user's full OS access. The docs are honest about it: five
website pages state plugins are "not OS-sandboxed", and a contract test
(`crates/flux-cli/tests/website_contract.rs` →
`plugin_security_copy_keeps_the_native_code_trust_boundary_explicit`) enforces that phrasing.
A malicious or compromised plugin binary can bypass the capability-callback protocol with direct
syscalls; a model-authored shell command can write anywhere the user can.

This epic adds an OS-level sandbox as **defense-in-depth underneath the envelope**: a
platform-abstracted wrapper applied at flux's single process choke point,
`System::build_command` (crates/flux-system/src/lib.rs — "the one place flux constructs an OS
process"). Because all five spawn modes funnel through it, one seam confines shell ops **and**
plugin subprocesses alike — flipping the "not OS-sandboxed" disclaimer into a feature. Backends:
**bubblewrap** on Linux, **Seatbelt** (`sandbox-exec`) on macOS, **graceful degradation** on
Windows (real backend is follow-up work).

Decisions fixed up front (user calls, 2026-07-10): bubblewrap-only on Linux; opt-in default-off
first (on-by-default is a later release); orthogonal to the approval gate in v1 (no
auto-approve-when-sandboxed coupling); `spawn_debug_pipe` (browser) exempt in v1.

## Approach

### Invariants (verify before ship)

1. **No bypass**: every sandboxed-mode spawn goes through `build_command`; the wrap happens
   inside it, so no caller can skip the sandbox without an explicit, greppable `Confinement::Exempt`.
2. **Kill semantics preserved**: `kill_on_drop` / `child.kill()` / `ProcessGroup::terminate`
   (killpg) must reach the real command through the wrapper. Linux: `--die-with-parent` +
   `--unshare-pid` guarantee namespace teardown; macOS: `sandbox-exec` execs in place (same pid).
3. **Plugin stdio intact**: the NDJSON capability-callback protocol over piped stdin/stdout must
   round-trip through the wrapper unchanged.
4. **Fail-closed under `require`**: `require = true` + no usable backend refuses to spawn — at
   startup preflight *and* as a per-spawn backstop.
5. **Nested runs don't double-wrap**: a flux child inside a flux sandbox detects the
   `FLUX_SANDBOXED` marker and skips re-wrapping (the outer namespaces already confine it).

### Abstraction — concrete enum, no trait

New module `crates/flux-system/src/sandbox.rs` (peer of `net.rs`; L2, no codegate change, no new
Rust deps — bwrap/sandbox-exec are external binaries). Key types:

- `SandboxSettings { mode: Off|On|Require, network: bool, extra_writable: Vec<PathBuf> }` with
  `from_env()` reading `FLUX_SANDBOX` / `FLUX_SANDBOX_NET` / `FLUX_SANDBOX_WRITABLE` — the
  inheritance channel, mirroring `FLUX_ADD_DIRS`/`FLUX_ALLOW_ALL`.
- `Backend::{ Bubblewrap { bwrap: PathBuf }, Seatbelt { sandbox_exec: PathBuf }, Unsupported { reason } }`
  — discovery stores the **absolute** wrapper path (a caller-supplied PATH override must never
  redirect which wrapper runs).
- `Sandbox { settings, backend }` with `resolve()` (platform pick + discovery, mirrors
  `discover_chrome`), `wrap_argv(argv, &SpawnPolicy)` (THE seam; identity when inactive),
  `configure(&mut Command)` (no-op v1; the future Windows pre-spawn API hook),
  `ensure_available()` (fail-closed backstop), `preflight()` (one-shot probe, cached).
- `SpawnPolicy { writable, network, cwd }` derived per spawn from the `Workspace`;
  `Confinement::{Sandboxed, Exempt}` passed explicitly by each spawn mode.

Two methods (`wrap_argv` argv-prefix + `configure` pre-spawn hook) accommodate both wrapper
styles, so a Windows backend later adds an enum variant without touching `build_command` again.

### Policy semantics (v1)

- **Read**: whole fs visible read-only (toolchains, /etc, TLS certs, locales just work).
- **Write**: workspace root + `@named` roots (the write-capable set per `Workspace::resolve`) +
  reciprocally validated linked-worktree Git administrative/common directories +
  `/tmp` & `$TMPDIR` + toolchain caches (`CARGO_HOME`/`~/.cargo`, `RUSTUP_HOME`/`~/.rustup` —
  SAFE_ENV already forwards these) + `[sandbox] writable` extras. Missing configured directories
  are created before launch and become required binds; writable `/` is rejected unless the
  workspace is explicitly unconfined. `read_roots` (`--add-dir`) are
  read-only and need no binds.
- **Network**: on/off whole-namespace (`[sandbox] network`, default on).
- `--allow-all-paths` lifts fs confinement from the profile too (warned); network policy still applies.
- Deliberately not in v1, not dead-ended: secret-path read masking (`~/.ssh`), per-spawn network
  variance, seccomp.

### Threading: config → CLI → env → System

- **flux-config**: `SandboxConfig { enabled, require, network: Option<bool>, writable: Vec<String> }`
  as a declared `Config` field (`deny_unknown_fields`) + merge arm. Merge is security-directional:
  `enabled`/`require` OR (a project may tighten, never loosen), `network` strictest-wins,
  `writable` concat (documented widening, like `add_dirs`).
- **flux-cli**: global `--sandbox` / `--no-sandbox` (conflicting); `apply_sandbox_env` beside
  `apply_workspace_access_env`, resolving flag > pre-set env > config and exporting `FLUX_SANDBOX`
  etc. so child flux invocations (`app run`, eval sub-agents, `plugin call`) inherit.
  `FLUX_SANDBOX=off` doubles as the kill switch (`FLUX_OP_CACHE` precedent). Preflight at startup
  when enabled: `require` + unavailable = startup error; otherwise one styled warning naming the
  reason (Windows v1 is exactly this path).
- **flux-system**: `System` gains a `sandbox: Sandbox` field. `System::new(workspace)` stays
  env-free/infallible (sandbox disabled — hermetic test sites untouched); new
  `System::from_env(cwd)` for production sites; `with_sandbox()` for custom-workspace sites.
  Sandboxed spawns get `cmd.env("FLUX_SANDBOXED", "1")`, and `FLUX_SANDBOXED` joins SAFE_ENV so
  the marker survives descendants' env-clear.

### Per-spawn-mode application

| Mode | v1 | Note |
|---|---|---|
| `run` / `run_with_env` | Sandboxed | shell/exec ops |
| `run_with_env_streamed` | Sandboxed | inherit-stdio passes through |
| `spawn_background` | Sandboxed | |
| `spawn_interactive` | **Sandboxed** | plugins — the headline |
| `spawn_debug_pipe` | **Exempt** | Chrome's own sandbox needs nested userns; forcing `--no-sandbox` on Chrome to fit inside bwrap is a net security loss. Browser confinement remains env-clear + CDP egress interception (D-124). Revisit in follow-up. |

### Linux backend (bubblewrap) — verified mechanics

Baseline argv (verified on bwrap 0.11.2 / kernel 6.6; all flags exist since bwrap ≤ 0.3):
`--die-with-parent --unshare-pid --unshare-ipc --unshare-uts --unshare-cgroup-try`
[+ `--unshare-net` iff network=off]
`--ro-bind / / --dev /dev --proc /proc --tmpfs /run`
[+ empty resolver parents and `--ro-bind-try` for individual systemd-resolved/resolvconf/
NetworkManager resolver files when network=on; no host IPC directory rebind]
`--bind /tmp /tmp --bind <ws-root> <ws-root>` [+ per-writable `--bind`/`--bind-try`]
`--chdir <ws-root> -- <original argv…>`

Empirically verified: `--tmpfs /run` is **mandatory** (with `--ro-bind / /` alone, docker.sock /
D-Bus / system sockets stay connectable even under `--unshare-net`); `--dev /dev` is mandatory
(`>/dev/null` fails EACCES otherwise); **no `--new-session`** (it would setsid the child out of
the PGID that `ProcessGroup::terminate` killpgs, and break inherited-tty modes; TIOCSTI is dead on
kernels ≥ 6.2); fd passthrough works (inherited fds preserved); env passes through untouched; exit
codes propagate (signal deaths surface as 128+n — benign). Probe: run the real baseline flag set
with `true`; classify ENOENT → Missing, "Operation not permitted / Creating new namespace failed"
→ NamespacesDenied (Debian ≤ 11 sysctl, Ubuntu 23.10+ AppArmor userns restriction, default-seccomp
Docker — the terminal-bench eval containers land here and must auto-degrade), other → Broken. The
inner `true` is resolved to an absolute executable from the caller's PATH (non-FHS-safe), and the
probe runs through `System::build_command`'s synchronous guarded mode with a process group, bounded
stderr, deadline, and descendant cleanup — never a second raw `Command` path.

### macOS backend (Seatbelt)

`sandbox-exec -D WS_ROOT=… -D TMP=… -p <profile>`: `(version 1)(allow default)(deny file-write*)`
+ `(allow file-write* (subpath (param "WS_ROOT")) (subpath (param "TMP")) (subpath "/private/tmp")
(subpath "/private/var/tmp") …)` + device carve-outs (`/dev/null`, `/dev/tty*`, `/dev/fd/`) +
`(deny network*)` when network=off. Paths must be canonicalized (`/tmp` → `/private/tmp`; TMPDIR
under `/var/folders`). Dynamic paths only via `-D` params, never string interpolation; reject `"`
in extra paths. `sandbox-exec` execs in place (same pid) so kill/process-group/exit-code semantics
are unchanged. Deprecated-but-ubiquitous (Bazel/Chromium-era mechanism); items needing real
hardware are flagged "verify on macOS" in D-136.

## Alternatives considered

- **`trait SandboxBackend`** — exactly one backend exists per platform; a trait adds dispatch and
  object-safety design for zero polymorphic call sites. Enum + two methods is simpler and Windows-ready.
- **New L0/L1 sandbox crate** — the launcher does process IO (not L0) and has exactly one consumer
  (flux-system); module-in-crate matches the repo's "one crate + modules" rule and needs no
  codegate change.
- **Landlock fallback on Linux** — native (no external binary) but fs-only; rejected for v1
  (user call: bubblewrap only).
- **Sandboxing the browser spawn too** — portable only by forcing Chrome `--no-sandbox`, trading a
  strong content sandbox for a weak outer one; exempted instead (documented).
- **Coupling to approvals (auto-approve sandboxed exec)** — big UX win but couples two security
  mechanisms in one wave; deferred to a follow-up story.

## Risks & open questions

- **Toolchain writes outside the workspace** (cargo registry, rustup) — top UX risk; mitigated by
  default-writable cache set + `[sandbox] writable` + opt-in default. Cache-poisoning tradeoff documented.
- **userns variance** (Docker CI, hardened distros) — preflight classification + `require` posture
  + `FLUX_SANDBOXED` marker skip.
- **Wrapper kill semantics** — `kill_on_drop` reaches only the wrapper; `--die-with-parent` +
  `--unshare-pid` are mandatory profile lines, backed by a no-orphan regression test.
- **`sandbox-exec` deprecation** — replaceable behind the Backend enum (e.g. a direct
  `sandbox_init` helper) if Apple removes it.
- **`deny_unknown_fields`**: configs with `[sandbox]` are rejected by older flux binaries — release-note it.
- **What v1 does NOT defend against** (goes in docs verbatim): secret *reads* anywhere on the fs
  (`~/.ssh` readable); exfiltration while network=on; shared-`/tmp` interference; cargo/rustup
  cache poisoning; anything on Windows.

Follow-ups named, not scheduled: Windows backend (AppContainer/Job Objects via `configure`),
on-by-default flip, approval-relaxation-when-sandboxed, secret-read masking + browser re-examination.

### Review remediation (2026-07-10)

An xhigh recall-mode review found the epic's biggest gap was **per-construction-site opt-in**: the
sandbox was attached only where a caller remembered `System::from_env`/`with_sandbox`, so `app run`,
the served agent, the SDK, and runtime git-context all defaulted to unconfined even under
`require`. Those sites now attach the env-resolved posture; terminal-bench eval spawns are the one
documented exemption (they drive Docker; the container is the boundary). The altitude lesson stands
as a **follow-up**: prefer resolving the posture at the `build_command` choke point (or a required
`System` constructor arg) over per-site threading, so a future `System::new` can't silently
re-introduce the gap. Other remediations: posture resolution is now **tightest-wins** (an on-mode
input can't downgrade a configured `require`; explicit `off` stays the kill switch); a genuinely
nested run resolves `Backend::AlreadyConfined` (satisfies `require`) instead of bricking; the
`FLUX_SANDBOXED` marker uses truthy semantics; the macOS probe degrades instead of panicking; the
preflight probe runs with a cleared env; DNS restoration covers resolvconf/NetworkManager; and
`SandboxConfig` rejects unknown keys.

A second review-remediation pass closed the remaining confinement/workflow gaps: malformed config
is now a hard startup error (so `require` cannot disappear before plugin-status/skill spawns); DNS
restores only resolver files and leaves D-Bus/NetworkManager/systemd-resolved sockets masked;
writable `/` is rejected or safely ordered through the explicit unconfined posture; configured
writable directories are created and required; linked worktree Git metadata joins the write set
only after reciprocal/layout validation; and the local-eval child flux host is explicitly exempt
from its child network namespace while receiving the resolved posture for its own shell/plugin
descendants. The terminal-bench Docker-driving host spawns remain the separate documented exemption.

## Acceptance / done

Union of D-134…D-137: sandbox abstraction + config + threading land with hermetic tests
(D-134); bubblewrap backend with golden-argv tests + live double-gated smokes proving
write-outside-workspace fails, network-off blocks loopback, plugin stdio round-trips sandboxed,
and kills leave no orphan (D-135); Seatbelt backend with golden-profile tests (D-136); website
security docs updated truthfully with the contract test rewritten (D-137). Gate green in both
workspaces throughout.
