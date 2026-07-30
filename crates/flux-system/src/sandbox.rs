//! OS-level process sandboxing — the abstraction, config, and threading seam (D-130).
//!
//! flux's safety envelope (authorization → approval → guarded IO) governs what the *model* may
//! request; this module adds an OS-level wrapper as defense-in-depth **underneath** it, applied at
//! [`crate::System`]'s single process choke point (`build_command`). [`Sandbox::resolve`] discovers
//! and functionally probes the platform backend — bubblewrap on Linux ([`Backend::Bubblewrap`],
//! D-131), Seatbelt on macOS ([`Backend::Seatbelt`], D-132) — falling back to
//! [`Backend::Unsupported`] when none is usable, or [`Backend::AlreadyConfined`] when an outer flux
//! sandbox already confines this process. On a platform that never grows a real backend (Windows
//! v1), only [`Backend::Unsupported`] is ever returned, and this module's behavior — settings
//! plumbing, warnings, fail-closed `require` — **is** the shipped behavior.
//!
//! See `docs/designs/process-sandboxing.md` for the full design (invariants, policy semantics,
//! backend mechanics).

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
#[cfg(any(test, target_os = "linux", target_os = "macos"))]
use std::sync::Mutex;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::sync::OnceLock;
#[cfg(any(test, target_os = "linux", target_os = "macos"))]
use std::time::Duration;

use flux_core::{Error, Result};

use crate::{env_truthy, Workspace};

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

/// The sandbox posture: off (no confinement attempted), on (confine when a backend is available,
/// warn and continue when it isn't), or require (confine, and refuse to spawn when it isn't —
/// [`Sandbox::ensure_available`]'s fail-closed backstop).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxMode {
    Off,
    On,
    Require,
}

/// The resolved sandbox posture for this process: mode, network policy, and any extra writable
/// paths layered on top of [`SpawnPolicy::for_workspace`]'s defaults.
#[derive(Debug, Clone)]
pub struct SandboxSettings {
    pub mode: SandboxMode,
    /// Whether sandboxed processes may reach the network. Default `true` (unrestricted) —
    /// narrowing is opt-in, mirroring the sandbox itself being opt-in.
    pub network: bool,
    /// Extra writable paths beyond the workspace root, named roots, `/tmp`/`$TMPDIR`, and the
    /// toolchain caches (`CARGO_HOME`/`~/.cargo`, `RUSTUP_HOME`/`~/.rustup`).
    pub extra_writable: Vec<PathBuf>,
}

impl SandboxSettings {
    /// The disabled posture: `Off`, unrestricted network, no extra writable paths. Used by
    /// [`Sandbox::disabled`] (the [`crate::System::new`] default — hermetic test sites untouched).
    pub fn off() -> Self {
        Self {
            mode: SandboxMode::Off,
            network: true,
            extra_writable: Vec::new(),
        }
    }

    /// Read the sandbox posture from the environment — the inheritance channel that lets a child
    /// `flux` invocation (`app run`, an eval sub-agent, `plugin call`) pick up the parent's posture
    /// without re-parsing CLI flags, mirroring [`Workspace::from_env`]'s `FLUX_ADD_DIRS`/
    /// `FLUX_ALLOW_ALL`:
    ///
    /// - `FLUX_SANDBOX` = `off` | `on` | `require` (case-insensitive; unset or any other value is
    ///   `off` — the safe default, since sandboxing is opt-in).
    /// - `FLUX_SANDBOX_NET`: a truthy value (`1`/`true`/`yes`/`on`) means the network stays open;
    ///   unset defaults to open too. The CLI only ever *exports* this when narrowing to `0`
    ///   (closed) — see `flux-cli`'s `apply_sandbox_env` — but any other value here is honored the
    ///   same way `FLUX_ALLOW_PRIVATE_NET` treats an explicit "off" value as authoritative.
    /// - `FLUX_SANDBOX_WRITABLE`: a `:`-separated list of extra writable paths (already
    ///   absolutized by the exporting CLI).
    pub fn from_env() -> Self {
        let mode = match std::env::var("FLUX_SANDBOX") {
            Ok(v) => match v.to_ascii_lowercase().as_str() {
                "on" => SandboxMode::On,
                "require" => SandboxMode::Require,
                _ => SandboxMode::Off,
            },
            Err(_) => SandboxMode::Off,
        };
        let network = std::env::var("FLUX_SANDBOX_NET")
            .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
            .unwrap_or(true);
        let extra_writable = std::env::var("FLUX_SANDBOX_WRITABLE")
            .map(|v| {
                v.split(':')
                    .filter(|s| !s.is_empty())
                    .map(PathBuf::from)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            mode,
            network,
            extra_writable,
        }
    }
}

// ---------------------------------------------------------------------------
// Backend
// ---------------------------------------------------------------------------

/// A discovered (or absent) sandbox implementation. Discovery always stores the **absolute**
/// wrapper path — a caller-supplied `PATH` override must never be able to redirect which binary
/// actually runs as the confining wrapper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    /// Linux: `bwrap` (bubblewrap), discovered via `FLUX_BWRAP_BIN` then `PATH` (D-131).
    Bubblewrap { bwrap: PathBuf },
    /// macOS: `/usr/bin/sandbox-exec` (Seatbelt, D-132).
    Seatbelt { sandbox_exec: PathBuf },
    /// This process is already running inside a flux sandbox (a truthy `FLUX_SANDBOXED` marker,
    /// see [`Sandbox::resolve`]): the outer namespaces already confine the whole tree, so this
    /// process adds no wrapper of its own — but it *is* confined, so `require` is satisfied and no
    /// "unavailable" warning is due. Distinct from [`Backend::Unsupported`], which is a genuine
    /// absence; surfaced by [`Sandbox::confined_by_parent`].
    AlreadyConfined,
    /// No usable backend: wrong platform, the wrapper binary is missing, or the preflight probe
    /// failed. `reason` is shown verbatim in the `require`-mode startup error and the `on`-mode
    /// warning.
    Unsupported { reason: String },
}

// ---------------------------------------------------------------------------
// Sandbox
// ---------------------------------------------------------------------------

/// One-shot latch behind [`Sandbox::take_posture_disclosure`]: the resolved-posture disclosure is a
/// per-**process** fact, so it is stated once no matter how many [`Sandbox`]es a process resolves or
/// clones. Process-global for the same reason [`PROBE_CACHE`] is — per-instance state would be
/// defeated by `Sandbox: Clone`.
static POSTURE_DISCLOSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Test-only reset of [`POSTURE_DISCLOSED`], so the once-per-process tests can each observe a fresh
/// latch. Callers hold [`SANDBOX_ENV_LOCK`] (via [`EnvGuard`]) to keep that observation exclusive.
#[cfg(test)]
fn reset_posture_disclosure_latch() {
    POSTURE_DISCLOSED.store(false, std::sync::atomic::Ordering::Relaxed);
}

/// The resolved sandbox for this process: settings plus the backend `resolve()` picked. The one
/// seam [`crate::System::build_command`] wraps through.
#[derive(Debug, Clone)]
pub struct Sandbox {
    settings: SandboxSettings,
    backend: Backend,
}

impl Sandbox {
    /// The disabled sandbox: [`crate::System::new`]'s default. Env-free and infallible so every
    /// hermetic test site that builds a `System` directly stays unaffected.
    pub fn disabled() -> Self {
        Self {
            settings: SandboxSettings::off(),
            backend: Backend::Unsupported {
                reason: "sandbox disabled".to_string(),
            },
        }
    }

    /// Resolve a [`Sandbox`] for `settings`: picks a platform backend and discovers it (D-131
    /// Linux/bubblewrap, D-132 macOS/Seatbelt). Discovery folds in the functional preflight probe
    /// (see [`probe_cached`]) — a binary that merely *exists* but doesn't actually work (most
    /// commonly `NamespacesDenied` inside default-seccomp Docker / hardened kernels, the
    /// terminal-bench eval containers' native habitat) resolves [`Backend::Unsupported`], not a
    /// backend an `On`-mode caller would then try and fail to use. This is a deliberate departure
    /// from a pure "does the binary exist" reading of `resolve()`: folding the probe in here (once,
    /// synchronously, cached) is what makes `on`-mode auto-degrade *before* any real spawn is
    /// attempted, and keeps [`Sandbox::is_active`]/[`Sandbox::reason`]/[`Sandbox::describe`]
    /// correct without needing to also consult a probe result — see the D-131 story notes.
    ///
    /// Skips discovery entirely (and stays `Unsupported`) when `settings.mode` is `Off` — sandboxing
    /// is opt-in, so a disabled sandbox must never pay for a `bwrap`/`sandbox-exec` probe.
    ///
    /// A process that is itself already running inside a flux sandbox (a *truthy* `FLUX_SANDBOXED`
    /// marker — see [`crate::System::build_command`] — matched with [`env_truthy`] semantics so a
    /// spoofed `FLUX_SANDBOXED=0` can't disable confinement) resolves [`Backend::AlreadyConfined`]:
    /// the outer namespaces already confine the whole tree, so re-wrapping would at best be a no-op
    /// and at worst fail outright (bwrap does not nest cleanly under `--unshare-pid`). Because it is
    /// genuinely confined, this satisfies `require` (see [`Sandbox::ensure_available`]) and is *not*
    /// an "unavailable" state — [`Sandbox::confined_by_parent`] distinguishes it from
    /// [`Backend::Unsupported`]. `Off` is checked first, so a disabled sandbox never re-interprets a
    /// stray marker as confinement.
    pub fn resolve(settings: SandboxSettings) -> Self {
        // Sandboxing is opt-in: a disabled sandbox must never pay for a probe, and must not be
        // re-read as "confined by a parent" just because a stray `FLUX_SANDBOXED` is set.
        if settings.mode == SandboxMode::Off {
            return Self {
                settings,
                backend: Backend::Unsupported {
                    reason: "sandbox disabled".to_string(),
                },
            };
        }
        if env_truthy("FLUX_SANDBOXED") {
            return Self {
                settings,
                backend: Backend::AlreadyConfined,
            };
        }
        let backend = discover_backend();
        Self { settings, backend }
    }

    /// The settings this sandbox was resolved from (needed by [`SpawnPolicy::for_workspace`]).
    pub fn settings(&self) -> &SandboxSettings {
        &self.settings
    }

    /// Whether this sandbox wraps a spawn with a backend of its own: the mode isn't `Off` AND a
    /// real backend ([`Backend::Bubblewrap`]/[`Backend::Seatbelt`]) was discovered and passed its
    /// preflight probe. `false` for [`Backend::Unsupported`] (no usable backend) and for
    /// [`Backend::AlreadyConfined`] (an outer flux sandbox already confines this process, so this
    /// process adds nothing — see [`Sandbox::confined_by_parent`]).
    pub fn is_active(&self) -> bool {
        self.settings.mode != SandboxMode::Off
            && matches!(
                self.backend,
                Backend::Bubblewrap { .. } | Backend::Seatbelt { .. }
            )
    }

    /// Whether this process is confined by an *outer* flux sandbox (a truthy `FLUX_SANDBOXED`
    /// marker → [`Backend::AlreadyConfined`]) rather than by a backend of its own. When true the
    /// process IS confined even though [`Sandbox::is_active`] is `false`, so the CLI suppresses the
    /// "sandbox unavailable" warning it would otherwise print for an inactive sandbox.
    pub fn confined_by_parent(&self) -> bool {
        matches!(self.backend, Backend::AlreadyConfined)
    }

    /// The reason a backend is unavailable, or `None` when one is active. Surfaced verbatim in the
    /// CLI's startup warning/error.
    pub fn reason(&self) -> Option<&str> {
        match &self.backend {
            Backend::Unsupported { reason } => Some(reason),
            _ => None,
        }
    }

    /// A one-line human-readable summary for status output / warnings.
    pub fn describe(&self) -> String {
        match (self.settings.mode, &self.backend) {
            (SandboxMode::Off, _) => "sandbox: off".to_string(),
            (_, Backend::Bubblewrap { .. }) => "sandbox: active (bubblewrap)".to_string(),
            (_, Backend::Seatbelt { .. }) => "sandbox: active (seatbelt)".to_string(),
            (_, Backend::AlreadyConfined) => "sandbox: confined by parent flux".to_string(),
            (mode, Backend::Unsupported { reason }) => {
                format!("sandbox: unavailable ({reason}) [mode={mode:?}]")
            }
        }
    }

    /// The **resolved-posture disclosure** (C-217): the one line an operator must be shown when they
    /// asked to be confined and are not. `Some` only for `On` + [`Backend::Unsupported`] — the single
    /// posture in which flux runs *unconfined despite having been asked to confine*.
    ///
    /// It states what is **true** (this process is running unconfined), not what was requested,
    /// because the failure mode this guards against is an operator who configured `on` believing it
    /// took effect. The `reason` is [`discover_backend`]'s, surfaced verbatim — this composes a
    /// disclosure, it does not compute a diagnosis.
    ///
    /// `None` everywhere else, because nothing is being withheld: `Off` never asked; a live
    /// [`Backend::Bubblewrap`]/[`Backend::Seatbelt`] confines this process; [`Backend::AlreadyConfined`]
    /// means an outer flux sandbox confines the whole tree; and `Require` + `Unsupported` never
    /// reaches an unconfined run at all — [`Sandbox::ensure_available`] fails closed first and that
    /// error *is* the disclosure. A line that fires when nothing is wrong trains operators to ignore
    /// the line that matters.
    ///
    /// Worded as a **posture statement, not an error.** The most common `reason` here is a refused
    /// user namespace ([`ProbeOutcome::NamespacesDenied`] — default-seccomp Docker, Debian ≤11's
    /// sysctl, Ubuntu 23.10+'s AppArmor userns restriction, and every terminal-bench eval
    /// container), which is an expected, healthy state on those hosts rather than a fault.
    ///
    /// Carries only posture + `reason`: no argv, no workspace layout, no secret. The `reason` may
    /// name an operator-supplied `FLUX_BWRAP_BIN`/`FLUX_SANDBOX_EXEC_BIN` path, exactly as
    /// [`Sandbox::describe`] and the `require` error already do — this adds no new disclosure class.
    ///
    /// This is the pure accessor: it answers every time it is asked, so an on-demand surface
    /// (`flux doctor`'s `sandbox backend` check) keeps working. For the unasked-for startup
    /// disclosure, which must appear at most once per process, use
    /// [`Sandbox::take_posture_disclosure`].
    pub fn posture_disclosure(&self) -> Option<String> {
        match (self.settings.mode, &self.backend) {
            (SandboxMode::On, Backend::Unsupported { reason }) => Some(format!(
                "sandbox: requested `on`, running UNCONFINED — no usable backend: {reason}. \
                 Shell/plugin processes get no OS-level confinement this run; set \
                 `[sandbox] require = true` (or `FLUX_SANDBOX=require`) to fail closed instead."
            )),
            _ => None,
        }
    }

    /// [`Sandbox::posture_disclosure`], but at most **once per process**: returns the line on the
    /// first call that has something to disclose and `None` on every call after it.
    ///
    /// This is what an unasked-for surface emits. Deliberately *not* per spawn — a warning on every
    /// [`Sandbox::wrap_argv`] would bury the signal in exactly the sessions that spawn most, which
    /// is the same "noise gets filtered, then missed" failure the wording above avoids.
    ///
    /// The latch is process-global (like [`PROBE_CACHE`]) rather than per-instance because
    /// [`Sandbox`] is `Clone` and a process may resolve several of them; one operator-visible fact
    /// deserves one operator-visible line. A sandbox with nothing to disclose does **not** consume
    /// the latch — otherwise the first [`Sandbox::disabled`] built in a process (every hermetic
    /// [`crate::System::new`]) would burn it and silence the real disclosure that follows.
    pub fn take_posture_disclosure(&self) -> Option<String> {
        // `?` first: only a sandbox that actually has something to say may consume the latch.
        let line = self.posture_disclosure()?;
        (!POSTURE_DISCLOSED.swap(true, std::sync::atomic::Ordering::Relaxed)).then_some(line)
    }

    /// The fail-closed backstop: `require` + no usable backend refuses to spawn, naming the
    /// reason. A no-op for `Off`/`On` — an `On` sandbox that can't confine **continues**, and the
    /// disclosure it owes is a separate, non-fallible concern ([`Sandbox::posture_disclosure`],
    /// emitted once per process at CLI startup by `flux-cli`'s `apply_sandbox_env`). Keeping the two
    /// apart is deliberate: `require`'s fail-closed contract must not depend on anything about
    /// reporting.
    pub fn ensure_available(&self) -> Result<()> {
        if self.settings.mode == SandboxMode::Require {
            if let Backend::Unsupported { reason } = &self.backend {
                return Err(Error::Config(format!(
                    "sandbox required (FLUX_SANDBOX=require / [sandbox] require) but unavailable: \
                     {reason}"
                )));
            }
        }
        Ok(())
    }

    /// One-shot startup probe. The real functional check (D-131/D-132: spawn the backend's
    /// baseline flag set against `true`/a minimal allow-all profile, classify
    /// `Missing`/`NamespacesDenied`/`Broken`) now runs *inside* [`Sandbox::resolve`] itself — see
    /// its rustdoc for why — and is cached there ([`probe_cached`], keyed by binary path), so by
    /// the time a `Sandbox` exists the probe has already happened at most once. `preflight` stays
    /// `async fn` (its established signature) and simply re-asserts [`Sandbox::ensure_available`],
    /// which by construction already reflects the cached probe outcome.
    pub async fn preflight(&self) -> Result<()> {
        self.ensure_available()
    }

    /// Pre-spawn hook for a backend that configures the child via the `Command` API rather than an
    /// argv prefix (the future Windows AppContainer/Job Objects backend). No-op in v1: both
    /// current backend families ([`Backend::Bubblewrap`], [`Backend::Seatbelt`]) are argv-prefix
    /// wrappers, handled entirely by [`Sandbox::wrap_argv`].
    pub(crate) fn configure(&self, _cmd: &mut std::process::Command) -> Result<()> {
        Ok(())
    }

    /// THE seam: prefix `argv` with the active backend's wrapper invocation under `policy`.
    /// Identity when inactive (mode `Off`, or no backend resolved). Delegates to
    /// [`bubblewrap_argv`]/[`seatbelt_argv`]. For Seatbelt only, first rejects any writable path
    /// containing a `"` or a control character ([`reject_unsafe_seatbelt_paths`]) — those paths get
    /// embedded directly into the generated SBPL profile *string*, so an unprintable byte or an
    /// unescaped quote is a profile-injection risk; bwrap's binds pass paths as separate execv
    /// argv entries (no string embedding), so no equivalent rejection is needed there.
    pub(crate) fn wrap_argv(&self, argv: &[String], policy: &SpawnPolicy) -> Result<Vec<String>> {
        if !self.is_active() {
            return Ok(argv.to_vec());
        }
        prepare_writable_paths(policy)?;
        match &self.backend {
            Backend::Bubblewrap { bwrap } => Ok(bubblewrap_argv(bwrap, argv, policy)),
            Backend::Seatbelt { sandbox_exec } => {
                reject_unsafe_seatbelt_paths(policy)?;
                Ok(seatbelt_argv(sandbox_exec, argv, policy))
            }
            // Never reached (both are `!is_active()`, gated above), but must be exhaustive and
            // must never panic: identity is the correct behavior for an already-confined process.
            Backend::AlreadyConfined => Ok(argv.to_vec()),
            Backend::Unsupported { .. } => unreachable!("is_active() already excluded this arm"),
        }
    }
}

// ---------------------------------------------------------------------------
// Backend discovery + preflight probing (D-131 bubblewrap, D-132 Seatbelt)
// ---------------------------------------------------------------------------

/// Discover and functionally probe this platform's backend. `#[cfg]`-gated per platform so a Linux
/// build never references `sandbox-exec` (and vice versa); the argv/profile builders below stay
/// `cfg`-free so their golden tests run everywhere.
fn discover_backend() -> Backend {
    #[cfg(target_os = "linux")]
    {
        match discover_bwrap() {
            Ok(bwrap) => match discover_probe_executable("true") {
                Ok(command) => match probe_cached(&bwrap, &bwrap_probe_argv(&command)) {
                    ProbeOutcome::Ok => Backend::Bubblewrap { bwrap },
                    ProbeOutcome::Missing(reason) => Backend::Unsupported { reason },
                    ProbeOutcome::NamespacesDenied => Backend::Unsupported {
                        reason: format!(
                            "{bwrap:?} exists but unprivileged user namespaces are refused by this \
                             kernel/policy (common inside Docker or hardened kernels — Debian ≤11's \
                             sysctl, Ubuntu 23.10+'s AppArmor userns restriction, default-seccomp \
                             containers): bubblewrap cannot confine here"
                        ),
                    },
                    ProbeOutcome::Broken(stderr) => Backend::Unsupported {
                        reason: format!("{bwrap:?} preflight probe failed: {stderr}"),
                    },
                },
                Err(reason) => Backend::Unsupported { reason },
            },
            Err(reason) => Backend::Unsupported { reason },
        }
    }
    #[cfg(target_os = "macos")]
    {
        match discover_sandbox_exec() {
            Ok(sandbox_exec) => match probe_cached(&sandbox_exec, &seatbelt_probe_argv()) {
                ProbeOutcome::Ok => Backend::Seatbelt { sandbox_exec },
                ProbeOutcome::Missing(reason) => Backend::Unsupported { reason },
                // Seatbelt has no namespace concept, so `run_probe` classifies on stderr patterns
                // that happen to overlap the userns-denial set (e.g. "Operation not permitted"
                // from a locked-down sandbox-exec). Treat it as `Broken` and degrade — a probe
                // that was refused must never abort startup with an `unreachable!`.
                ProbeOutcome::NamespacesDenied => Backend::Unsupported {
                    reason: format!(
                        "{sandbox_exec:?} preflight probe was refused (operation not permitted): \
                         sandbox-exec cannot confine here"
                    ),
                },
                ProbeOutcome::Broken(stderr) => Backend::Unsupported {
                    reason: format!("{sandbox_exec:?} preflight probe failed: {stderr}"),
                },
            },
            Err(reason) => Backend::Unsupported { reason },
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Backend::Unsupported {
            reason: "no sandbox backend exists for this platform yet".to_string(),
        }
    }
}

/// First match for `name` on `PATH`, returned as-is (not yet canonicalized — callers that need the
/// absolute-path invariant canonicalize themselves). Mirrors `flux-web::browser::which_on_path`.
#[cfg(target_os = "macos")]
fn which_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    which_on_path_in(name, &path)
}

/// [`which_on_path`] with an injected PATH value. Keeping the split/lookup seam pure lets parallel
/// tests cover discovery without replacing the process environment seen by unrelated child spawns.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn which_on_path_in(name: &str, path: &std::ffi::OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        .map(|dir| dir.join(name))
        .find(|cand| cand.is_file())
}

/// Resolve the command executed *inside* a backend probe through the caller's PATH, then pin it to
/// an absolute path before the probe environment is scrubbed. Hard-coding `/usr/bin:/bin` breaks
/// otherwise-functional bubblewrap installs on non-FHS systems such as NixOS and Guix.
#[cfg(target_os = "linux")]
fn discover_probe_executable(name: &str) -> std::result::Result<PathBuf, String> {
    let path = std::env::var_os("PATH");
    discover_probe_executable_in(name, path.as_deref())
}

#[cfg(target_os = "linux")]
fn discover_probe_executable_in(
    name: &str,
    path: Option<&std::ffi::OsStr>,
) -> std::result::Result<PathBuf, String> {
    path.and_then(|path| which_on_path_in(name, path))
        .and_then(|path| path.canonicalize().ok())
        .ok_or_else(|| format!("probe command `{name}` not found on the caller's PATH"))
}

/// Discover the bubblewrap binary: `FLUX_BWRAP_BIN` (must exist) → `PATH` lookup for `bwrap`.
/// Always canonicalizes to an **absolute** path — the wrapper argv[0] must never be a bare name a
/// `PATH` swap (or a spawn whose `PATH` was cleared) could redirect.
#[cfg(target_os = "linux")]
fn discover_bwrap() -> std::result::Result<PathBuf, String> {
    let override_bin = std::env::var_os("FLUX_BWRAP_BIN");
    let path = std::env::var_os("PATH");
    discover_bwrap_in(override_bin.as_deref(), path.as_deref())
}

#[cfg(target_os = "linux")]
fn discover_bwrap_in(
    override_bin: Option<&std::ffi::OsStr>,
    path: Option<&std::ffi::OsStr>,
) -> std::result::Result<PathBuf, String> {
    if let Some(p) = override_bin.filter(|p| !p.is_empty()) {
        return std::fs::canonicalize(p)
            .map_err(|e| format!("FLUX_BWRAP_BIN={p:?} is not a usable bwrap binary: {e}"));
    }
    path.and_then(|path| which_on_path_in("bwrap", path))
        .and_then(|p| std::fs::canonicalize(&p).ok())
        .ok_or_else(|| {
            "bubblewrap (bwrap) not found on PATH — install it or set FLUX_BWRAP_BIN".to_string()
        })
}

/// Discover the Seatbelt binary: `FLUX_SANDBOX_EXEC_BIN` (must exist) → the fixed
/// `/usr/bin/sandbox-exec` location → `PATH` lookup. Always canonicalizes to an absolute path (same
/// rationale as [`discover_bwrap`]).
#[cfg(target_os = "macos")]
fn discover_sandbox_exec() -> std::result::Result<PathBuf, String> {
    if let Some(p) = std::env::var_os("FLUX_SANDBOX_EXEC_BIN").filter(|p| !p.is_empty()) {
        return std::fs::canonicalize(&p).map_err(|e| {
            format!("FLUX_SANDBOX_EXEC_BIN={p:?} is not a usable sandbox-exec binary: {e}")
        });
    }
    let fixed = PathBuf::from("/usr/bin/sandbox-exec");
    if let Ok(p) = std::fs::canonicalize(&fixed) {
        return Ok(p);
    }
    which_on_path("sandbox-exec")
        .and_then(|p| std::fs::canonicalize(&p).ok())
        .ok_or_else(|| {
            "sandbox-exec not found at /usr/bin/sandbox-exec or on PATH — install Xcode command \
             line tools or set FLUX_SANDBOX_EXEC_BIN"
                .to_string()
        })
}

/// The classification of a functional preflight probe against a discovered backend binary.
/// Distinguishes "genuinely broken" from "the OS refuses the confinement primitive itself" — the
/// latter (`NamespacesDenied`) is the *expected* state inside default-seccomp Docker / hardened
/// kernels (the terminal-bench eval containers land here) and must let an `on`-mode caller degrade
/// to unconfined rather than treat it as a hard failure; only `require` mode turns it into an error
/// (via [`Sandbox::ensure_available`], since [`discover_backend`] resolves it as `Unsupported`).
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Debug, Clone, PartialEq, Eq)]
enum ProbeOutcome {
    Ok,
    Missing(String),
    NamespacesDenied,
    Broken(String),
}

/// stderr substrings bwrap/the kernel emit when unprivileged user-namespace creation is refused —
/// Debian ≤11's sysctl, Ubuntu 23.10+'s AppArmor userns restriction, and default-seccomp Docker all
/// land here (see the design doc's "Linux backend" section).
#[cfg(any(target_os = "linux", target_os = "macos"))]
const NAMESPACE_DENIAL_PATTERNS: [&str; 4] = [
    "Operation not permitted",
    "Creating new namespace failed",
    "setting up uid map",
    "No permissions to create",
];

/// The bubblewrap baseline flag set used for the preflight probe against an absolute, caller-PATH-
/// resolved `true`: the lifecycle/fs flags that are present regardless of a specific
/// [`SpawnPolicy`] (no writable binds — the probe
/// only asks "does this kernel let bwrap create its namespaces at all", not "can it bind this
/// workspace"). The command is absolute so the guarded launcher's scrubbed environment does not
/// assume an FHS `/usr/bin:/bin` layout.
#[cfg(target_os = "linux")]
fn bwrap_probe_argv(command: &Path) -> Vec<String> {
    [
        "--die-with-parent",
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
        "--unshare-cgroup-try",
        "--ro-bind",
        "/",
        "/",
        "--dev",
        "/dev",
        "--proc",
        "/proc",
        "--tmpfs",
        "/run",
        "--",
    ]
    .into_iter()
    .map(String::from)
    .chain(std::iter::once(path_str(command)))
    .collect()
}

/// `sandbox-exec` probe argv: a minimal allow-all profile against a real command, matching the
/// D-132 acceptance's `-p '(version 1)(allow default)' /usr/bin/true`.
#[cfg(target_os = "macos")]
fn seatbelt_probe_argv() -> Vec<String> {
    ["-p", "(version 1)(allow default)", "/usr/bin/true"]
        .into_iter()
        .map(String::from)
        .collect()
}

/// Max wall-clock time the preflight probe waits for the backend to exit before treating it as
/// `Broken` (a hung probe must not hang `resolve()` — startup would never complete).
#[cfg(any(target_os = "linux", target_os = "macos"))]
const PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(2);

/// Global preflight-probe cache, keyed by the discovered binary's absolute path — so `resolve()`
/// (called once per `System`, but a process may build several `System`s, and tests construct many)
/// pays the real subprocess-spawn cost at most once per distinct binary. A `Mutex<HashMap<..>>`
/// rather than per-`Sandbox`-instance state: `Sandbox` is `Clone`, and the whole point of caching is
/// to survive across clones/re-resolves within the same process.
#[cfg(any(target_os = "linux", target_os = "macos"))]
static PROBE_CACHE: OnceLock<Mutex<HashMap<PathBuf, ProbeOutcome>>> = OnceLock::new();

/// Run (or fetch the cached result of) a preflight probe: spawn `bin` with the full `args` (the
/// probed command, e.g. `true`/`/usr/bin/true`, is already the last element — see
/// `bwrap_probe_argv`/`seatbelt_probe_argv`). Cached by `bin`'s path.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn probe_cached(bin: &Path, args: &[String]) -> ProbeOutcome {
    let cache = PROBE_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(outcome) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(bin) {
        return outcome.clone();
    }
    let outcome = run_probe(bin, args, PREFLIGHT_TIMEOUT);
    cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(bin.to_path_buf(), outcome.clone());
    outcome
}

/// The actual probe runs through [`crate::System`]'s synchronous guarded-launcher mode: the same
/// `build_command` choke point, safe environment, process group, bounded capture, and descendant
/// cleanup as product spawns, without assuming a Tokio runtime exists during startup resolution.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn run_probe(bin: &Path, args: &[String], timeout: Duration) -> ProbeOutcome {
    let argv: Vec<String> = std::iter::once(path_str(bin))
        .chain(args.iter().cloned())
        .collect();
    let output = match crate::System::run_guarded_probe(&argv, timeout) {
        Ok(output) => output,
        Err(crate::GuardedProbeError::Spawn(err)) if err.kind() == std::io::ErrorKind::NotFound => {
            return ProbeOutcome::Missing(format!("{bin:?} not found: {err}"));
        }
        Err(crate::GuardedProbeError::Spawn(err)) => {
            return ProbeOutcome::Missing(format!("{bin:?} could not be spawned: {err}"));
        }
        Err(crate::GuardedProbeError::Other(reason)) => return ProbeOutcome::Broken(reason),
    };
    if output.timed_out {
        return ProbeOutcome::Broken(format!("probe timed out after {timeout:?}"));
    }
    if output.status.is_some_and(|status| status.success()) {
        return ProbeOutcome::Ok;
    }
    let stderr = output.stderr.trim().to_string();
    if NAMESPACE_DENIAL_PATTERNS
        .iter()
        .any(|pattern| stderr.contains(pattern))
    {
        ProbeOutcome::NamespacesDenied
    } else {
        ProbeOutcome::Broken(stderr)
    }
}

// ---------------------------------------------------------------------------
// SpawnPolicy
// ---------------------------------------------------------------------------

/// The per-spawn confinement policy derived from a [`Workspace`] + [`SandboxSettings`]: what a
/// wrapped process may write to, whether it may reach the network, and its working directory. Read
/// access is deliberately not modeled here — v1's read policy is "whole filesystem, read-only"
/// (toolchains/`/etc`/TLS certs/locales just work), so every backend applies it uniformly rather
/// than deriving it per spawn.
#[derive(Debug, Clone)]
pub struct SpawnPolicy {
    pub writable: Vec<PathBuf>,
    /// The explicitly configured subset of [`Self::writable`]. Unlike automatic best-effort cache
    /// roots, these paths are created before wrapping and emitted as required binds so a declared
    /// `[sandbox] writable` entry can never be silently ignored.
    configured_writable: Vec<PathBuf>,
    pub network: bool,
    pub cwd: PathBuf,
    /// Mirrors [`Workspace::is_unconfined`] — the `--allow-all-paths` hatch. When set, a backend
    /// lifts *filesystem* confinement entirely (bwrap: `--bind / /` instead of the read-only root
    /// plus per-path write binds; Seatbelt: skip the `deny file-write*`/allow-subpath block
    /// entirely). Network policy is unaffected — `--allow-all-paths` lifts fs confinement only, the
    /// design doc is explicit that network policy still applies on top of it.
    pub unconfined: bool,
}

impl SpawnPolicy {
    /// Derive the write-capable set for `workspace` under `settings`: the workspace root, every
    /// `@named` root (the same write-capable set [`Workspace::resolve`] honors), validated linked
    /// Git-worktree administrative/common roots, `/tmp` and
    /// `$TMPDIR`, the toolchain caches (`CARGO_HOME` or `~/.cargo`; `RUSTUP_HOME` or `~/.rustup` —
    /// `SAFE_ENV` already forwards these into the child's environment, so the sandbox must let it
    /// write there or every `cargo`/`rustup` invocation breaks under confinement), and finally
    /// `settings.extra_writable` (the `[sandbox] writable` / `FLUX_SANDBOX_WRITABLE` escape hatch,
    /// prepared as required directories by [`prepare_writable_paths`]).
    /// Deduplicated but NOT canonicalized — a backend's argv builder resolves symlinks itself if it
    /// needs to (bwrap/sandbox-exec both accept non-canonical paths).
    pub fn for_workspace(workspace: &Workspace, settings: &SandboxSettings) -> Self {
        let mut writable = vec![workspace.root().to_path_buf()];
        writable.extend(workspace.named_roots().map(Path::to_path_buf));
        writable.extend(linked_worktree_writable_roots(workspace.root()));
        writable.push(PathBuf::from("/tmp"));
        if let Ok(tmpdir) = std::env::var("TMPDIR") {
            if !tmpdir.is_empty() {
                writable.push(PathBuf::from(tmpdir));
            }
        }
        writable.push(cargo_home());
        writable.push(rustup_home());
        writable.extend(settings.extra_writable.iter().cloned());
        // Drop empty or relative entries: an unset `HOME` (→ `.cargo`/`.rustup`) or an explicit
        // `CARGO_HOME=""`/`RUSTUP_HOME=""` would otherwise emit a `--bind-try "" ""` or an empty
        // `(subpath (param "Wn"))` — a nonsensical (bwrap) or profile-corrupting (Seatbelt) bind.
        // Every generated writable root is absolute. Configured entries retain their original copy
        // in `configured_writable`, so an active sandbox rejects (rather than silently accepts) a
        // relative misconfiguration in `prepare_writable_paths`.
        writable.retain(|p| p.is_absolute());
        writable.sort();
        writable.dedup();
        Self {
            writable,
            configured_writable: settings.extra_writable.clone(),
            network: settings.network,
            cwd: workspace.root().to_path_buf(),
            // A workspace rooted at `/` is already filesystem-unconfined by definition. Treat it
            // like the explicit hatch so bwrap emits `--bind / /` *before* restoring /dev, /proc,
            // and the masked /run tmpfs rather than remounting the host tree over those protections.
            unconfined: workspace.is_unconfined() || workspace.root() == Path::new("/"),
        }
    }
}

/// Resolve the external Git metadata needed by a linked worktree. Git writes its index/HEAD under
/// `<common>/worktrees/<id>` and refs/objects under `<common>`, both outside the worktree root.
///
/// The `.git` pointer is workspace-writable, so it is not trusted on its own: accepting an arbitrary
/// `gitdir: /etc` would turn the next sandbox spawn into a write-confinement bypass. Admit an
/// external directory only when it has Git's reciprocal `gitdir` backpointer to this worktree and
/// its `commondir` resolves to the standard direct `<common>/worktrees/<id>` layout.
fn linked_worktree_writable_roots(worktree: &Path) -> Vec<PathBuf> {
    let dot_git = worktree.join(".git");
    let Some(admin) = read_gitdir_pointer(&dot_git, worktree) else {
        return Vec::new();
    };

    let backpointer = admin.join("gitdir");
    let Some(backpointer_target) = read_path_file(&backpointer, &admin) else {
        return Vec::new();
    };
    let Ok(dot_git_canonical) = dot_git.canonicalize() else {
        return Vec::new();
    };
    if backpointer_target != dot_git_canonical {
        return Vec::new();
    }

    let Some(common) = read_path_file(&admin.join("commondir"), &admin) else {
        return Vec::new();
    };
    let standard_layout = admin
        .parent()
        .filter(|parent| parent.file_name().is_some_and(|name| name == "worktrees"))
        .and_then(Path::parent)
        .is_some_and(|expected| expected == common);
    if !standard_layout {
        return Vec::new();
    }
    vec![admin, common]
}

fn read_gitdir_pointer(dot_git: &Path, base: &Path) -> Option<PathBuf> {
    let text = read_small_git_metadata(dot_git)?;
    let raw = text.lines().next()?.strip_prefix("gitdir:")?.trim();
    resolve_existing_path(raw, base)
}

fn read_path_file(path: &Path, base: &Path) -> Option<PathBuf> {
    let text = read_small_git_metadata(path)?;
    resolve_existing_path(text.trim(), base)
}

fn read_small_git_metadata(path: &Path) -> Option<String> {
    const MAX_GIT_POINTER_BYTES: u64 = 4096;
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_GIT_POINTER_BYTES {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

fn resolve_existing_path(raw: &str, base: &Path) -> Option<PathBuf> {
    if raw.is_empty() {
        return None;
    }
    let path = Path::new(raw);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    path.canonicalize().ok()
}

/// Validate the writable set before backend argv construction and materialize explicitly configured
/// output roots. Automatic roots such as absent cargo/rustup caches stay best-effort; configured
/// roots are a contract and therefore either exist (created as directories) or fail clearly.
fn prepare_writable_paths(policy: &SpawnPolicy) -> Result<()> {
    if !policy.unconfined
        && policy
            .writable
            .iter()
            .any(|path| writable_path_is_root(path))
    {
        return Err(Error::Config(
            "sandbox writable root `/` is not allowed because a late root bind would replace the \
             protected /dev, /proc, and /run mounts; use --allow-all-paths for the explicit, safely \
             ordered unconfined posture"
                .to_string(),
        ));
    }

    for path in &policy.configured_writable {
        if path.as_os_str().is_empty() || !path.is_absolute() {
            return Err(Error::Config(format!(
                "configured sandbox writable path {path:?} must be absolute"
            )));
        }
        match std::fs::metadata(path) {
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir_all(path).map_err(|create_err| {
                    Error::Config(format!(
                        "create configured sandbox writable directory {path:?}: {create_err}"
                    ))
                })?;
            }
            Err(err) => {
                return Err(Error::Config(format!(
                    "inspect configured sandbox writable path {path:?}: {err}"
                )))
            }
        }
    }
    Ok(())
}

fn writable_path_is_root(path: &Path) -> bool {
    crate::normalize_lexically(path) == Path::new("/")
        || path.canonicalize().is_ok_and(|path| path == Path::new("/"))
}

fn cargo_home() -> PathBuf {
    // Treat an empty `CARGO_HOME` as unset (mirrors the `TMPDIR` handling in `for_workspace`), so
    // `CARGO_HOME=""` falls back to `~/.cargo` rather than yielding an empty writable path.
    std::env::var("CARGO_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".cargo"))
}

fn rustup_home() -> PathBuf {
    std::env::var("RUSTUP_HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".rustup"))
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Confinement
// ---------------------------------------------------------------------------

/// Whether a given spawn goes through the sandbox at all — an explicit, greppable parameter at
/// each of [`crate::System`]'s five spawn-mode call sites, so no caller can silently skip
/// confinement (invariant 1 in the design doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confinement {
    /// Wrapped when the sandbox is active: `run`/`run_with_env`, `run_with_env_streamed`,
    /// `spawn_background`, `spawn_interactive`.
    Sandboxed,
    /// Never wrapped. Used only at explicit trusted-host seams: `spawn_debug_pipe` (Chrome's own
    /// content sandbox needs a nested user namespace), the local-eval child flux host (it needs
    /// provider network access and receives the posture for its own descendants), and the backend
    /// preflight (which is testing the wrapper itself). Every exemption remains argv-only,
    /// env-cleared, workspace-pinned, and guarded for cleanup.
    Exempt,
}

/// The `(key, value)` env override a spawn gets once it is genuinely wrapped — split out from
/// `build_command` so the decision is unit-testable without a live backend. `None` for an
/// `Exempt` spawn or an inactive sandbox: the marker must only ever claim confinement that
/// genuinely happened, because a nested child trusts it to skip re-wrapping.
pub(crate) fn sandbox_marker(
    confinement: Confinement,
    sandbox: &Sandbox,
) -> Option<(&'static str, &'static str)> {
    (confinement == Confinement::Sandboxed && sandbox.is_active())
        .then_some(("FLUX_SANDBOXED", "1"))
}

/// The `FLUX_SANDBOX` spelling of a mode — the exact vocabulary [`SandboxSettings::from_env`]
/// parses back, so a rendered posture round-trips into the same posture.
fn mode_env_value(mode: SandboxMode) -> &'static str {
    match mode {
        SandboxMode::Off => "off",
        SandboxMode::On => "on",
        SandboxMode::Require => "require",
    }
}

/// C-276: the posture a spawn hands to its child, alongside the `FLUX_SANDBOXED` marker
/// [`sandbox_marker`] injects. Split out from `apply_safe_env` so the decision is unit-testable
/// without a live backend.
///
/// The defect this closes was an **asymmetry**: `SAFE_ENV` carried `FLUX_SANDBOXED` — the marker
/// whose whole job is to assert *"you are already confined"* — and none of the variables that
/// decide whether confinement happens. A spawned `flux` therefore resolved its posture from an
/// environment containing no posture, got `off` (the opt-in default), and so declined to confine
/// its own descendants while the operator had demanded `require`. Forwarding a claim of
/// confinement without the means to enforce it is strictly worse than forwarding nothing.
///
/// **Every value here is rendered from `sandbox`, never read back out of `std::env`.** That is not
/// a stylistic choice. `System::with_sandbox` exists so an embedder can pin a posture *independent
/// of the ambient environment* (`flux-sdk`'s `SystemBuilder`: "pass one only to pin a posture
/// independent of ambient env"), so the two sources legitimately disagree. Deciding *whether* to
/// forward from the resolved sandbox while taking *what* to forward from the environment produced
/// exactly the failure this function exists to prevent: a pinned `On` sandbox under an ambient
/// `FLUX_SANDBOX=off` passed the gate and then handed the child the kill switch — leaving it less
/// confined than forwarding nothing at all. One source, or the guarantee is fiction.
///
/// The posture travels as a **floor, never a ceiling**. Two consequences, and both are load-bearing
/// because on the reading side these values do not merely *inform* a child, they *beat* it:
/// - An `Off` sandbox forwards **nothing** — not even `FLUX_SANDBOX=off`. `off` is not "no
///   opinion"; it is `flux-cli`'s explicit kill switch, which short-circuits ahead of a child's own
///   `[sandbox] require` *and* C-262's unattended fail-closed profile.
/// - An **open** network forwards nothing either. `FLUX_SANDBOX_NET` is emitted only to say
///   *closed*, because a truthy value likewise beats both `[sandbox] network` and C-262's
///   unattended-closed default. An unrestricted network is the absence of a restriction, and
///   absence is not something a parent gets to impose. This mirrors `flux-cli`'s own exporter,
///   which writes the variable when narrowing and otherwise leaves it alone.
///
/// Withholding in both cases leaves the child free to resolve its own (possibly stricter) posture,
/// which is exactly the pre-C-276 behaviour. So for those two keys — and **only** those two — what
/// this function returns can never loosen a child; read the `FLUX_SANDBOX_WRITABLE` bullet below
/// for the one that is a union rather than a narrowing. The guarantee is a property of *this
/// function*, not of the whole spawn path: `apply_safe_env` applies a caller's explicit `env`
/// overrides after this, and a call site is free to push anything it likes into that slot.
///
/// Why each key is safe to add to the allow-list, against the deny-by-default rule that flux never
/// forwards a host credential to a child:
/// - `FLUX_SANDBOX` / `FLUX_SANDBOX_NET`: an `off|on|require` enum and a flag. Controls, not
///   values, and per the floor rule they can only tighten.
/// - `FLUX_SANDBOX_WRITABLE`: the extra writable set **this process resolved**, which is also the
///   set it just bound into the child's own wrapper via [`SpawnPolicy::for_workspace`]. This one is
///   *not* a strict narrowing, and saying so matters: `flux-cli`'s reader **unions** the forwarded
///   list with the child's own `[sandbox] writable` and de-dupes it, so a child that would have
///   confined its descendants to its own roots now also grants the parent's. That is bounded, not
///   an escape — every path in it is already writable in the envelope the child itself is running
///   under, in both the wrapped and the `Exempt` case, so a grandchild cannot reach outside what
///   the parent already permitted. Same category as the already-forwarded `PATH`/`HOME`/
///   `KUBECONFIG` — a filename is not a credential.
/// - `FLUX_BWRAP_BIN` / `FLUX_SANDBOX_EXEC_BIN`: the **absolute path discovery resolved and the
///   preflight probe verified** — the wrapper this process actually runs, not whatever the
///   environment asked for. A sandbox with no backend of its own ([`Backend::Unsupported`], or
///   [`Backend::AlreadyConfined`], which needs none) forwards neither, and the child discovers for
///   itself; it cannot inherit a wrapper this process never established. Forwarding the verified
///   path is strictly better than dropping it, since the child's fallback is a `PATH` lookup and
///   `PATH` is already forwarded — dropping it would hand wrapper selection to the weaker channel.
pub(crate) fn posture_env(sandbox: &Sandbox) -> Vec<(&'static str, String)> {
    let settings = sandbox.settings();
    if settings.mode == SandboxMode::Off {
        return Vec::new();
    }
    let mut out = vec![("FLUX_SANDBOX", mode_env_value(settings.mode).to_string())];
    if !settings.network {
        out.push(("FLUX_SANDBOX_NET", "0".to_string()));
    }
    if !settings.extra_writable.is_empty() {
        // `:`-joined, the separator `SandboxSettings::from_env` splits on. A path containing `:`
        // cannot survive that channel — a pre-existing property of the variable, not of this hop.
        let joined = settings
            .extra_writable
            .iter()
            .map(|p| path_str(p))
            .collect::<Vec<_>>()
            .join(":");
        out.push(("FLUX_SANDBOX_WRITABLE", joined));
    }
    match &sandbox.backend {
        Backend::Bubblewrap { bwrap } => out.push(("FLUX_BWRAP_BIN", path_str(bwrap))),
        Backend::Seatbelt { sandbox_exec } => {
            out.push(("FLUX_SANDBOX_EXEC_BIN", path_str(sandbox_exec)))
        }
        Backend::AlreadyConfined | Backend::Unsupported { .. } => {}
    }
    out
}

// ---------------------------------------------------------------------------
// Backend argv builders (D-131 bubblewrap, D-132 Seatbelt)
// ---------------------------------------------------------------------------

/// Render a path as a `String` for argv/profile embedding. Lossy on non-UTF-8 (matches the rest of
/// this crate's stance — `flux_system::path_to_utf8` — but infallible, since [`bubblewrap_argv`]/
/// [`seatbelt_argv`] are themselves infallible by design; a non-UTF-8 workspace path is already an
/// edge case flux doesn't otherwise support).
fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Build the bubblewrap wrapper prefix for `argv` under `policy`: the verified baseline template
/// (design doc "Linux backend" section) — lifecycle namespaces, optional network namespace, the
/// read-only whole-fs bind plus mandatory `/dev`/`/proc`/`/run` mounts, then either a single
/// `--bind / /` (policy.unconfined — the `--allow-all-paths` hatch: fs confinement fully lifted,
/// but lifecycle/network/`/run`-masking stay in force) or the workspace + writable-set binds.
fn bubblewrap_argv(bwrap: &Path, argv: &[String], policy: &SpawnPolicy) -> Vec<String> {
    let mut out = vec![path_str(bwrap)];

    // Lifecycle: die with the parent and get fresh pid/ipc/uts/cgroup namespaces regardless of
    // fs/network policy — `--unshare-pid` is also what makes `--die-with-parent` a real guarantee
    // (invariant 2: kill_on_drop/killpg must reach the wrapped process; a nested pid namespace that
    // dies with its parent can't leave an orphan behind).
    out.push("--die-with-parent".to_string());
    out.push("--unshare-pid".to_string());
    out.push("--unshare-ipc".to_string());
    out.push("--unshare-uts".to_string());
    out.push("--unshare-cgroup-try".to_string());
    if !policy.network {
        out.push("--unshare-net".to_string());
    }

    if policy.unconfined {
        // The `--allow-all-paths` hatch: fs confinement collapses to a single read-write bind of
        // the whole tree instead of ro-bind-plus-write-binds.
        out.push("--bind".to_string());
        out.push("/".to_string());
        out.push("/".to_string());
    } else {
        out.push("--ro-bind".to_string());
        out.push("/".to_string());
        out.push("/".to_string());
    }
    // Mandatory regardless of unconfined: `--unshare-pid` needs a fresh `/proc` to reflect the new
    // pid namespace, and `--dev` gives a working /dev/null etc (a plain bind of the host `/dev`
    // under `--unshare-*` leaves device nodes non-functional; empirically `>/dev/null` fails EACCES
    // without it — design doc).
    out.push("--dev".to_string());
    out.push("/dev".to_string());
    out.push("--proc".to_string());
    out.push("/proc".to_string());
    // Mandatory regardless of unconfined/network: without masking `/run`, docker.sock/D-Bus/other
    // system sockets under it stay connectable even under `--unshare-net` (design doc) — this is
    // about local socket reachability, not fs-write confinement, so it applies even when `--bind /
    // /` already gave full read-write access back.
    out.push("--tmpfs".to_string());
    out.push("/run".to_string());
    if policy.network {
        // DNS still needs to resolve when the network namespace is shared: /etc/resolv.conf is a
        // symlink into /run on most distros, and the tmpfs mount above just wiped /run. Re-expose
        // the common resolver *files* so the symlink target survives. Create only empty destination
        // parents inside the masked /run, then bind the distro-specific files individually.
        // Deliberately do not restore `/run/dbus`, `/run/NetworkManager`, or even the whole
        // `/run/systemd/resolve` directory (which contains systemd-resolved IPC sockets): directory
        // binds would expose host IPC after the `/run` mask. `--ro-bind-try` is a no-op when a source
        // file is absent, so listing several targets is safe.
        for dir in [
            "/run/systemd",
            "/run/systemd/resolve",
            "/run/resolvconf",
            "/run/NetworkManager",
        ] {
            out.push("--dir".to_string());
            out.push(dir.to_string());
        }
        for resolver in [
            "/run/systemd/resolve/resolv.conf",
            "/run/systemd/resolve/stub-resolv.conf",
            "/run/resolvconf/resolv.conf",
            "/run/NetworkManager/resolv.conf",
            "/run/NetworkManager/no-stub-resolv.conf",
        ] {
            out.push("--ro-bind-try".to_string());
            out.push(resolver.to_string());
            out.push(resolver.to_string());
        }
    }

    if !policy.unconfined {
        let cwd = path_str(&policy.cwd);
        let mut bound: HashSet<PathBuf> = HashSet::new();

        out.push("--bind".to_string());
        out.push("/tmp".to_string());
        out.push("/tmp".to_string());
        bound.insert(PathBuf::from("/tmp"));

        // The workspace root must exist (`Workspace::new` already requires it) — a real `--bind`,
        // not `--bind-try`, so a typo'd/missing root fails loudly instead of silently running with
        // an empty writable workspace.
        out.push("--bind".to_string());
        out.push(cwd.clone());
        out.push(cwd);
        bound.insert(policy.cwd.clone());

        // The rest of the writable set includes named/Git roots, /tmp/$TMPDIR, toolchain caches,
        // and `[sandbox] writable` extras. Automatic roots are best-effort (`--bind-try`) because a
        // machine may have no cargo/rustup cache. Explicitly configured roots were created by
        // `prepare_writable_paths` and use `--bind`, so a disappearance race fails the spawn loudly
        // instead of silently nullifying the operator's setting.
        for p in &policy.writable {
            if bound.insert(p.clone()) {
                let s = path_str(p);
                let configured = policy.configured_writable.iter().any(|configured| {
                    crate::normalize_lexically(configured) == crate::normalize_lexically(p)
                });
                out.push(if configured { "--bind" } else { "--bind-try" }.to_string());
                out.push(s.clone());
                out.push(s);
            }
        }
    }

    out.push("--chdir".to_string());
    out.push(path_str(&policy.cwd));
    out.push("--".to_string());
    out.extend(argv.iter().cloned());
    out
}

/// Reject a [`SpawnPolicy`]'s writable paths (and `cwd`) if any contains a `"` or a control
/// character. Seatbelt-only: [`seatbelt_argv`] embeds these paths directly into a generated SBPL
/// profile *string* via `-D NAME=value`, so an unescaped quote could break out of the intended
/// `(subpath (param "NAME"))` context, and a control character (e.g. an embedded newline) is at
/// best a confusing profile and at worst a parser-dependent surprise. bwrap's binds pass paths as
/// separate execv argv entries — there is no string to escape out of — so no equivalent check
/// applies there.
fn reject_unsafe_seatbelt_paths(policy: &SpawnPolicy) -> Result<()> {
    let mut candidates: Vec<&Path> = vec![policy.cwd.as_path()];
    candidates.extend(policy.writable.iter().map(PathBuf::as_path));
    for p in candidates {
        let s = p.to_string_lossy();
        if s.contains('"') || s.chars().any(|c| c.is_control()) {
            return Err(Error::Config(format!(
                "sandbox writable path {p:?} contains a `\"` or a control character, which is \
                 unsafe to embed in a Seatbelt SBPL profile string"
            )));
        }
    }
    Ok(())
}

/// Canonicalize `p` for profile emission (design doc: `/tmp` → `/private/tmp`, `TMPDIR` under
/// `/var/folders`); an automatic path that doesn't exist (for example a toolchain cache default)
/// falls back to the original, lexically-as-given path rather than erroring — an SBPL `subpath`
/// rule for a nonexistent path is inert, not unsafe. Configured roots are created beforehand.
fn canonicalize_for_profile(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Build the generated SBPL profile string: `(version 1)(allow default)` plus, unless
/// `unconfined`, a `deny file-write*` narrowed back open only under `WS_ROOT`/`TMP`/the fixed
/// `/private/tmp`+`/private/var/tmp` roots/`W0..Wn` extras, with device carve-outs so stdio-style
/// writes (`/dev/null`, ttys, inherited fds) keep working; then `deny network*` unless
/// `policy.network`. `extra_count` is the number of `Wn` params [`seatbelt_argv`] emitted.
fn seatbelt_profile(network: bool, unconfined: bool, extra_count: usize) -> String {
    let mut sb = String::from("(version 1)(allow default)");
    if !unconfined {
        sb.push_str("(deny file-write*)");
        sb.push_str(
            "(allow file-write* (subpath (param \"WS_ROOT\")) (subpath (param \"TMP\")) \
             (subpath \"/private/tmp\") (subpath \"/private/var/tmp\")",
        );
        for i in 0..extra_count {
            sb.push_str(&format!(" (subpath (param \"W{i}\"))"));
        }
        sb.push(')');
        sb.push_str(
            "(allow file-write-data (literal \"/dev/null\") (literal \"/dev/zero\") \
             (regex #\"^/dev/tty\") (regex #\"^/dev/fd/\"))",
        );
    }
    if !network {
        sb.push_str("(deny network*)");
    }
    sb
}

/// Build the Seatbelt (`sandbox-exec`) wrapper prefix for `argv` under `policy` (design doc "macOS
/// backend" section): `-D`-parameterized dynamic paths (never string-interpolated into the profile
/// — see [`reject_unsafe_seatbelt_paths`], run by [`Sandbox::wrap_argv`] before this), then `-p
/// <profile>`, then the original `argv` directly. No `--` separator: `sandbox-exec`'s own CLI
/// grammar is `sandbox-exec [-n name|-p profile|-f file] [-D key=value]... command [args...]` —
/// unlike bwrap it does not recognize (or need) an end-of-options marker, since `-D`/`-p`/`-n`/`-f`
/// are its only flags and the wrapped program is simply the next positional argument.
fn seatbelt_argv(sandbox_exec: &Path, argv: &[String], policy: &SpawnPolicy) -> Vec<String> {
    let mut out = vec![path_str(sandbox_exec)];

    let ws_root = canonicalize_for_profile(&policy.cwd);
    let tmp = canonicalize_for_profile(
        &std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| PathBuf::from("/tmp")),
    );

    out.push("-D".to_string());
    out.push(format!("WS_ROOT={}", path_str(&ws_root)));
    out.push("-D".to_string());
    out.push(format!("TMP={}", path_str(&tmp)));

    let mut extra_count = 0usize;
    if !policy.unconfined {
        let mut seen: HashSet<PathBuf> = HashSet::new();
        seen.insert(policy.cwd.clone());
        seen.insert(PathBuf::from("/tmp"));
        seen.insert(PathBuf::from("/private/tmp"));
        seen.insert(PathBuf::from("/private/var/tmp"));
        if let Some(tmpdir) = std::env::var_os("TMPDIR").map(PathBuf::from) {
            if !tmpdir.as_os_str().is_empty() {
                seen.insert(tmpdir);
            }
        }
        for p in &policy.writable {
            if seen.insert(p.clone()) {
                let canon = canonicalize_for_profile(p);
                out.push("-D".to_string());
                out.push(format!("W{extra_count}={}", path_str(&canon)));
                extra_count += 1;
            }
        }
    }

    out.push("-p".to_string());
    out.push(seatbelt_profile(
        policy.network,
        policy.unconfined,
        extra_count,
    ));
    out.extend(argv.iter().cloned());
    out
}

// ---------------------------------------------------------------------------
// Shared test-only env harness (reachable crate-wide)
// ---------------------------------------------------------------------------

/// Serializes tests that mutate the real `FLUX_SANDBOX*`/`FLUX_SANDBOXED`/`FLUX_BWRAP_BIN`/`PATH`
/// env vars — the process env is shared across parallel test threads, so two concurrent
/// `set_var`/`remove_var` calls on the SAME key race and flake (mirrors `flux-config`'s
/// `HOME_LOCK`). `pub(crate)` so the sandbox-touching tests over in `crate::tests` (`lib.rs`) take
/// the SAME single lock instance as this module's own tests, instead of racing it with an
/// unsynchronized one of their own.
#[cfg(test)]
pub(crate) static SANDBOX_ENV_LOCK: Mutex<()> = Mutex::new(());

/// Takes [`SANDBOX_ENV_LOCK`] and restores every touched env var to its prior value on drop —
/// panic-safe cleanup so a failed assertion can't leak a posture into a later test in the same
/// process. `pub(crate)` so `lib.rs`'s env-mutating sandbox tests reuse it verbatim (FIX G).
#[cfg(test)]
pub(crate) struct EnvGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

#[cfg(test)]
impl EnvGuard {
    pub(crate) fn new(keys: &[&'static str]) -> Self {
        let lock = SANDBOX_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = keys.iter().map(|&k| (k, std::env::var_os(k))).collect();
        for &k in keys {
            std::env::remove_var(k);
        }
        HOLDS_ENV_LOCK.with(|h| h.set(true));
        Self { _lock: lock, saved }
    }
}

#[cfg(test)]
impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, v) in &self.saved {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
        HOLDS_ENV_LOCK.with(|h| h.set(false));
    }
}

#[cfg(test)]
thread_local! {
    /// True while this thread holds [`SANDBOX_ENV_LOCK`] through a live [`EnvGuard`]. The lock is a
    /// plain non-reentrant `Mutex`, so [`fixture_path`] asks this instead of blindly acquiring it a
    /// second time and deadlocking a test that is already inside a guard.
    static HOLDS_ENV_LOCK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Takes [`SANDBOX_ENV_LOCK`] for the caller's scope, unless this thread is already inside an
/// [`EnvGuard`] — the lock is not reentrant, so a second acquire on the same thread would deadlock.
/// This is what lets every env-reading test helper be called from anywhere without case analysis.
#[cfg(test)]
pub(crate) fn env_lock_if_free() -> Option<EnvGuard> {
    if HOLDS_ENV_LOCK.with(std::cell::Cell::get) {
        None
    } else {
        Some(EnvGuard::new(&[]))
    }
}

/// A per-process-unique fixture name, `flux-<prefix>-<pid>-<seq>`.
#[cfg(test)]
fn fixture_name(prefix: &str) -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!(
        "flux-{prefix}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

/// **The one way this crate's tests may build a fixture root** (C-209). Reads the system temp dir
/// under [`SANDBOX_ENV_LOCK`] and hangs a unique name off it; the path is not created.
///
/// Sandbox tests deliberately mutate `TMPDIR` under an [`EnvGuard`] —
/// `wrap_argv_rejects_root_from_automatic_tmpdir_too` sets it to `/`. A bare `temp_dir()` read in
/// another test thread can therefore observe that transient value and root its fixture under it;
/// the owning test then restores `TMPDIR` and the victim fails on a path it never chose, as a bare
/// `Permission denied`/`No such file or directory` in a *different* test each run. Reading the base
/// under the same lock makes the capture impossible, which is why no bare read may return to the
/// test modules — `no_bare_temp_dir_in_the_test_modules` in `lib.rs` enforces that.
#[cfg(test)]
pub(crate) fn fixture_path(prefix: &str) -> PathBuf {
    let _env = env_lock_if_free();
    std::env::temp_dir().join(fixture_name(prefix))
}

/// [`fixture_path`], created on disk.
#[cfg(test)]
pub(crate) fn fixture_dir(prefix: &str) -> PathBuf {
    let dir = fixture_path(prefix);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_workspace() -> (PathBuf, Workspace) {
        let dir = fixture_dir("sandbox-test");
        let ws = Workspace::new(&dir).unwrap();
        (dir, ws)
    }

    /// **The one way these tests may build a policy** (C-209, second leg).
    /// [`SpawnPolicy::for_workspace`] reads `TMPDIR`/`CARGO_HOME`/`RUSTUP_HOME`/`HOME` from the
    /// process env, so a bare call while `wrap_argv_rejects_root_from_automatic_tmpdir_too` has
    /// `TMPDIR` swapped to `/` yields a posture nobody configured — and the caller then fails on a
    /// writable root it never asked for, in a different test each run. Read them under the same
    /// lock the mutators hold; `env_lock_if_free` keeps this safe inside a test that holds its own
    /// [`EnvGuard`]. `no_bare_temp_dir_in_the_test_modules` in `lib.rs` keeps this the only call.
    fn workspace_policy(ws: &Workspace, settings: &SandboxSettings) -> SpawnPolicy {
        let _env = env_lock_if_free();
        SpawnPolicy::for_workspace(ws, settings)
    }

    // `EnvGuard`/`SANDBOX_ENV_LOCK` now live at module scope (`pub(crate)`, above) so `lib.rs`'s
    // env-mutating sandbox tests share this exact lock instance — see FIX G. `use super::*` pulls
    // them into scope here unchanged.

    // -- SandboxSettings::from_env ------------------------------------------------------------

    #[test]
    fn from_env_defaults_off_with_open_network_and_no_extra_writable() {
        let _g = EnvGuard::new(&["FLUX_SANDBOX", "FLUX_SANDBOX_NET", "FLUX_SANDBOX_WRITABLE"]);
        let s = SandboxSettings::from_env();
        assert_eq!(s.mode, SandboxMode::Off);
        assert!(s.network, "network defaults open (unrestricted)");
        assert!(s.extra_writable.is_empty());
    }

    #[test]
    fn from_env_reads_mode_case_insensitively() {
        let _g = EnvGuard::new(&["FLUX_SANDBOX"]);
        for (raw, want) in [
            ("on", SandboxMode::On),
            ("ON", SandboxMode::On),
            ("require", SandboxMode::Require),
            ("Require", SandboxMode::Require),
            ("off", SandboxMode::Off),
            ("garbage", SandboxMode::Off),
        ] {
            std::env::set_var("FLUX_SANDBOX", raw);
            assert_eq!(
                SandboxSettings::from_env().mode,
                want,
                "FLUX_SANDBOX={raw:?}"
            );
        }
    }

    #[test]
    fn from_env_reads_network_truthy_value_not_mere_presence() {
        let _g = EnvGuard::new(&["FLUX_SANDBOX_NET"]);
        std::env::set_var("FLUX_SANDBOX_NET", "0");
        assert!(!SandboxSettings::from_env().network, "0 closes the network");
        std::env::set_var("FLUX_SANDBOX_NET", "1");
        assert!(SandboxSettings::from_env().network);
        // FIX F: truthiness is matched case-insensitively (mirrors the `FLUX_SANDBOX` mode parse),
        // so an uppercase `TRUE` is honored, not silently treated as "not truthy" → closed.
        std::env::set_var("FLUX_SANDBOX_NET", "TRUE");
        assert!(
            SandboxSettings::from_env().network,
            "uppercase TRUE is truthy (case-insensitive)"
        );
        std::env::set_var("FLUX_SANDBOX_NET", "On");
        assert!(
            SandboxSettings::from_env().network,
            "mixed-case On is truthy"
        );
        std::env::remove_var("FLUX_SANDBOX_NET");
        assert!(SandboxSettings::from_env().network, "unset stays open");
    }

    #[test]
    fn from_env_splits_writable_list() {
        let _g = EnvGuard::new(&["FLUX_SANDBOX_WRITABLE"]);
        std::env::set_var("FLUX_SANDBOX_WRITABLE", "/a:/b:/c");
        let s = SandboxSettings::from_env();
        assert_eq!(
            s.extra_writable,
            vec![
                PathBuf::from("/a"),
                PathBuf::from("/b"),
                PathBuf::from("/c")
            ]
        );
    }

    // -- Sandbox::resolve / nested marker ------------------------------------------------------

    /// A process already running inside a flux sandbox (truthy `FLUX_SANDBOXED`) resolves
    /// [`Backend::AlreadyConfined`] regardless of its own posture: the outer namespaces already
    /// confine the tree, so even under `require` it must satisfy [`Sandbox::ensure_available`]
    /// (nothing to re-wrap), report `is_active() == false` (this process adds nothing), and expose
    /// [`Sandbox::confined_by_parent`] so the CLI suppresses its "unavailable" warning (FIX A).
    #[test]
    fn resolve_under_flux_sandboxed_marker_is_confined_by_parent_and_satisfies_require() {
        let _g = EnvGuard::new(&["FLUX_SANDBOXED", "FLUX_SANDBOX"]);
        std::env::set_var("FLUX_SANDBOXED", "1");
        std::env::set_var("FLUX_SANDBOX", "require");
        let sandbox = Sandbox::resolve(SandboxSettings::from_env());
        assert!(sandbox.confined_by_parent());
        assert!(!sandbox.is_active());
        assert_eq!(sandbox.reason(), None);
        assert!(
            sandbox.ensure_available().is_ok(),
            "already confined by an outer sandbox satisfies `require`"
        );
        assert_eq!(sandbox.describe(), "sandbox: confined by parent flux");
    }

    /// FIX B: the `FLUX_SANDBOXED` marker is honored with truthy semantics (via [`env_truthy`]),
    /// not mere presence — a spoofed `FLUX_SANDBOXED=0` must NOT be read as "already confined" and
    /// disable the sandbox; discovery of a real/attempted backend proceeds instead.
    #[test]
    fn resolve_treats_falsey_flux_sandboxed_marker_as_unset() {
        let _g = EnvGuard::new(&["FLUX_SANDBOXED", "FLUX_BWRAP_BIN", "FLUX_SANDBOX_EXEC_BIN"]);
        std::env::set_var("FLUX_SANDBOXED", "0");
        let settings = SandboxSettings {
            mode: SandboxMode::On,
            network: true,
            extra_writable: Vec::new(),
        };
        let sandbox = Sandbox::resolve(settings);
        assert!(
            !sandbox.confined_by_parent(),
            "FLUX_SANDBOXED=0 must not count as parent confinement"
        );
    }

    #[test]
    fn resolve_without_marker_yields_platform_reason() {
        let _g = EnvGuard::new(&["FLUX_SANDBOXED"]);
        std::env::remove_var("FLUX_SANDBOXED");
        let sandbox = Sandbox::resolve(SandboxSettings::off());
        assert_ne!(sandbox.reason(), Some("already inside a flux sandbox"));
        assert!(sandbox.reason().is_some());
    }

    // -- disabled / ensure_available / wrap_argv identity --------------------------------------

    #[test]
    fn disabled_is_never_active_and_ensure_available_always_ok() {
        let sandbox = Sandbox::disabled();
        assert!(!sandbox.is_active());
        assert!(sandbox.ensure_available().is_ok());
    }

    /// The fail-closed backstop named in the acceptance: `require` + `Unsupported` refuses,
    /// naming the reason in the error.
    #[test]
    fn ensure_available_fails_closed_under_require_when_unsupported() {
        let sandbox = Sandbox {
            settings: SandboxSettings {
                mode: SandboxMode::Require,
                network: true,
                extra_writable: Vec::new(),
            },
            backend: Backend::Unsupported {
                reason: "no bwrap on PATH".to_string(),
            },
        };
        let err = sandbox.ensure_available().unwrap_err();
        assert!(err.to_string().contains("no bwrap on PATH"), "{err}");
    }

    /// `on` + no usable backend **continues** (that half was never in doubt) — but it must not
    /// continue *silently*. C-217 split the two halves apart: `ensure_available` stays the
    /// fail-closed backstop and keeps returning `Ok(())` here (the `continue` half, unchanged), and
    /// the disclosure the operator is owed is a separate, non-fallible concern
    /// ([`Sandbox::posture_disclosure`], asserted below). Both halves are pinned in one test so a
    /// future change cannot quietly restore the silence by deleting only the disclosure assertion.
    #[test]
    fn ensure_available_is_ok_under_on_mode_when_unsupported() {
        let sandbox = Sandbox {
            settings: SandboxSettings {
                mode: SandboxMode::On,
                network: true,
                extra_writable: Vec::new(),
            },
            backend: Backend::Unsupported {
                reason: "no bwrap on PATH".to_string(),
            },
        };
        assert!(sandbox.ensure_available().is_ok());
        assert!(
            sandbox.posture_disclosure().is_some(),
            "`on` + Unsupported continues, but it must disclose that it is running unconfined"
        );
    }

    // -- C-217: the resolved-posture disclosure ------------------------------------------------

    /// C-217: `on` + [`Backend::Unsupported`] is the one posture where flux runs unconfined
    /// *despite having been asked to confine*, so it owes the operator a line naming what is
    /// **true** (running unconfined) plus the reason `discover_backend` already computed — not a
    /// restatement of what was requested.
    #[test]
    fn posture_disclosure_names_the_resolved_posture_and_the_reason_under_on_mode() {
        let sandbox = Sandbox {
            settings: SandboxSettings {
                mode: SandboxMode::On,
                network: true,
                extra_writable: Vec::new(),
            },
            backend: Backend::Unsupported {
                reason: "no bwrap on PATH".to_string(),
            },
        };
        let line = sandbox
            .posture_disclosure()
            .expect("`on` + Unsupported must disclose its resolved posture");
        assert!(
            line.contains("UNCONFINED"),
            "the line must state the RESOLVED posture, not the requested one: {line}"
        );
        assert!(
            line.contains("no bwrap on PATH"),
            "the line must carry the reason discovery already computed: {line}"
        );
        assert!(
            line.contains("`on`"),
            "the line must name the posture that was requested, for contrast: {line}"
        );
        // A posture statement, not an error: `NamespacesDenied` (default-seccomp Docker, Debian ≤11,
        // Ubuntu 23.10+ AppArmor, every terminal-bench eval container) is an expected, healthy
        // state, so the wording must not read as a fault.
        let lower = line.to_ascii_lowercase();
        assert!(
            !lower.contains("error") && !lower.contains("failed"),
            "must read as a posture statement, not a fault: {line}"
        );
        // One line — a multi-line banner would not survive being interleaved with agent output.
        assert!(!line.contains('\n'), "must be exactly one line: {line}");
    }

    /// C-217: silent wherever confinement actually holds or was never requested. A warning that
    /// fires when nothing is wrong trains operators to ignore it, so every one of these must be
    /// `None`: `Off` (never asked), a live backend (confined by us), [`Backend::AlreadyConfined`]
    /// (confined by an outer flux sandbox — the
    /// `resolve_under_flux_sandboxed_marker_is_confined_by_parent_and_satisfies_require` path), and
    /// `Require` + `Unsupported` (never reaches an unconfined run at all — `ensure_available`
    /// fails closed first, and that error is itself the disclosure).
    #[test]
    fn posture_disclosure_is_silent_when_confinement_holds_or_was_never_requested() {
        let unsupported = || Backend::Unsupported {
            reason: "no bwrap on PATH".to_string(),
        };
        let with = |mode: SandboxMode, backend: Backend| Sandbox {
            settings: SandboxSettings {
                mode,
                network: true,
                extra_writable: Vec::new(),
            },
            backend,
        };
        let bwrap = Backend::Bubblewrap {
            bwrap: PathBuf::from("/usr/bin/bwrap"),
        };
        let seatbelt = Backend::Seatbelt {
            sandbox_exec: PathBuf::from("/usr/bin/sandbox-exec"),
        };

        for (label, sandbox) in [
            (
                "off never asked to be confined",
                with(SandboxMode::Off, unsupported()),
            ),
            (
                "a live bubblewrap backend confines us",
                with(SandboxMode::On, bwrap.clone()),
            ),
            (
                "a live seatbelt backend confines us",
                with(SandboxMode::On, seatbelt.clone()),
            ),
            (
                "an outer flux sandbox already confines us",
                with(SandboxMode::On, Backend::AlreadyConfined),
            ),
            (
                "require + unsupported fails closed instead",
                with(SandboxMode::Require, unsupported()),
            ),
            (
                "require + a live backend is confined",
                with(SandboxMode::Require, bwrap),
            ),
            (
                "require under an outer sandbox is confined",
                with(SandboxMode::Require, Backend::AlreadyConfined),
            ),
        ] {
            assert_eq!(
                sandbox.posture_disclosure(),
                None,
                "must stay silent: {label}"
            );
        }

        // The default `System::new` sandbox is `Off`, so no hermetic test site gains a line.
        assert_eq!(Sandbox::disabled().posture_disclosure(), None);
    }

    /// C-217: the disclosure is emitted **once per process, not per spawn** — a per-`wrap_argv`
    /// warning would bury the signal in exactly the sessions that spawn most. The latch is
    /// process-global (like [`PROBE_CACHE`]) rather than per-`Sandbox`, because `Sandbox` is
    /// `Clone` and a process may resolve several of them.
    #[test]
    fn take_posture_disclosure_yields_the_line_at_most_once_per_process() {
        // Takes the shared sandbox env lock so this test cannot interleave with another test that
        // consumes the same process-global latch.
        let _g = EnvGuard::new(&["FLUX_SANDBOXED"]);
        reset_posture_disclosure_latch();

        let unconfined = Sandbox {
            settings: SandboxSettings {
                mode: SandboxMode::On,
                network: true,
                extra_writable: Vec::new(),
            },
            backend: Backend::Unsupported {
                reason: "no bwrap on PATH".to_string(),
            },
        };

        assert!(
            unconfined.take_posture_disclosure().is_some(),
            "the first take discloses"
        );
        assert_eq!(
            unconfined.take_posture_disclosure(),
            None,
            "a second take in the same process must stay silent"
        );
        // A *different* `Sandbox` (a clone, or a re-resolve) shares the one latch.
        assert_eq!(
            unconfined.clone().take_posture_disclosure(),
            None,
            "the latch is process-global, not per-instance"
        );
        // The pure accessor is unaffected by the latch — it stays available for `flux doctor`-style
        // on-demand surfaces that must report the posture every time they are asked.
        assert!(unconfined.posture_disclosure().is_some());
    }

    /// C-217: a sandbox with nothing to disclose must NOT consume the latch — otherwise the first
    /// `Sandbox::disabled()` built in a process (every hermetic `System::new`) would burn it and
    /// silence the real disclosure that follows.
    #[test]
    fn take_posture_disclosure_does_not_burn_the_latch_when_there_is_nothing_to_say() {
        let _g = EnvGuard::new(&["FLUX_SANDBOXED"]);
        reset_posture_disclosure_latch();

        assert_eq!(Sandbox::disabled().take_posture_disclosure(), None);
        let confined = Sandbox {
            settings: SandboxSettings {
                mode: SandboxMode::On,
                network: true,
                extra_writable: Vec::new(),
            },
            backend: Backend::AlreadyConfined,
        };
        assert_eq!(confined.take_posture_disclosure(), None);

        let unconfined = Sandbox {
            settings: SandboxSettings {
                mode: SandboxMode::On,
                network: true,
                extra_writable: Vec::new(),
            },
            backend: Backend::Unsupported {
                reason: "no bwrap on PATH".to_string(),
            },
        };
        assert!(
            unconfined.take_posture_disclosure().is_some(),
            "a silent sandbox must not have consumed the one-shot latch"
        );
    }

    #[test]
    fn wrap_argv_is_identity_when_inactive() {
        let (_dir, ws) = temp_workspace();
        let sandbox = Sandbox::disabled();
        let policy = workspace_policy(&ws, sandbox.settings());
        let argv = vec!["echo".to_string(), "hi".to_string()];
        let wrapped = sandbox.wrap_argv(&argv, &policy).unwrap();
        assert_eq!(wrapped, argv);
    }

    // -- sandbox_marker (the FLUX_SANDBOXED injection decision) --------------------------------

    #[test]
    fn sandbox_marker_is_none_when_inactive_or_exempt() {
        let inactive = Sandbox::disabled();
        assert_eq!(sandbox_marker(Confinement::Sandboxed, &inactive), None);

        // Even an `active` (fake, test-only) backend is not marked when the spawn is Exempt.
        let active = Sandbox {
            settings: SandboxSettings {
                mode: SandboxMode::On,
                network: true,
                extra_writable: Vec::new(),
            },
            backend: Backend::Bubblewrap {
                bwrap: PathBuf::from("/usr/bin/bwrap"),
            },
        };
        assert_eq!(sandbox_marker(Confinement::Exempt, &active), None);
    }

    #[test]
    fn sandbox_marker_fires_only_for_sandboxed_confinement_over_an_active_backend() {
        let active = Sandbox {
            settings: SandboxSettings {
                mode: SandboxMode::On,
                network: true,
                extra_writable: Vec::new(),
            },
            backend: Backend::Bubblewrap {
                bwrap: PathBuf::from("/usr/bin/bwrap"),
            },
        };
        assert_eq!(
            sandbox_marker(Confinement::Sandboxed, &active),
            Some(("FLUX_SANDBOXED", "1"))
        );
    }

    // -- posture_env (C-276: the marker never travels alone) ------------------------------------

    /// Build a `Sandbox` directly, bypassing `resolve` — these tests are about what a *given*
    /// resolved posture hands on, with no host, env or backend probe in the loop.
    fn pinned(mode: SandboxMode, network: bool, extra: &[&str], backend: Backend) -> Sandbox {
        Sandbox {
            settings: SandboxSettings {
                mode,
                network,
                extra_writable: extra.iter().map(PathBuf::from).collect(),
            },
            backend,
        }
    }

    /// A confining sandbox hands its posture on **as values, not as key names**. Asserted
    /// exhaustively rather than by membership: a setting added to `SandboxSettings` and forgotten
    /// here recreates this story's defect — a child told it is confined without being told with
    /// what.
    ///
    /// Note what the wrapper path is: the absolute binary *discovery resolved and the probe
    /// verified*, which is what this process actually runs. It is not an echo of `FLUX_BWRAP_BIN`,
    /// and no ambient environment is consulted to produce any of these.
    #[test]
    fn a_posture_travels_whole_so_a_child_can_enforce_what_the_marker_claims() {
        let confining = pinned(
            SandboxMode::Require,
            false,
            &["/output", "/scratch"],
            Backend::Bubblewrap {
                bwrap: PathBuf::from("/nix/store/abc/bin/bwrap"),
            },
        );
        assert_eq!(
            posture_env(&confining),
            vec![
                ("FLUX_SANDBOX", "require".to_string()),
                ("FLUX_SANDBOX_NET", "0".to_string()),
                ("FLUX_SANDBOX_WRITABLE", "/output:/scratch".to_string()),
                ("FLUX_BWRAP_BIN", "/nix/store/abc/bin/bwrap".to_string()),
            ]
        );

        // macOS resolves the other wrapper; same rule, same source.
        let seatbelt = pinned(
            SandboxMode::On,
            false,
            &[],
            Backend::Seatbelt {
                sandbox_exec: PathBuf::from("/usr/bin/sandbox-exec"),
            },
        );
        assert_eq!(
            posture_env(&seatbelt),
            vec![
                ("FLUX_SANDBOX", "on".to_string()),
                ("FLUX_SANDBOX_NET", "0".to_string()),
                ("FLUX_SANDBOX_EXEC_BIN", "/usr/bin/sandbox-exec".to_string()),
            ]
        );
    }

    /// A sandbox with no wrapper of its own hands on the *request* and no wrapper path: it cannot
    /// pass down a binary it never established. `On`-but-unavailable still forwards the mode, so a
    /// child that does have a backend honors what this process asked for and could not do.
    #[test]
    fn a_sandbox_without_a_backend_forwards_the_request_but_no_wrapper_path() {
        let unbacked = pinned(
            SandboxMode::On,
            true,
            &[],
            Backend::Unsupported {
                reason: "bwrap not found".to_string(),
            },
        );
        assert_eq!(
            posture_env(&unbacked),
            vec![("FLUX_SANDBOX", "on".to_string())]
        );

        // Already inside an outer flux sandbox: the marker carries that fact, and a nested process
        // needs no wrapper path because it will not wrap.
        let nested = pinned(SandboxMode::Require, true, &[], Backend::AlreadyConfined);
        assert_eq!(
            posture_env(&nested),
            vec![("FLUX_SANDBOX", "require".to_string())]
        );
    }

    /// Floor-never-ceiling, first consequence: an `Off` sandbox forwards NOTHING rather than
    /// `FLUX_SANDBOX=off`. On the reading side `off` is not "no opinion" — it is `flux-cli`'s kill
    /// switch, which short-circuits ahead of a child's own `[sandbox] require` and C-262's
    /// unattended fail-closed profile. Handing it down would make this fix a new bypass channel.
    #[test]
    fn an_off_sandbox_forwards_no_posture_so_a_parent_can_never_downgrade_a_child() {
        assert!(posture_env(&Sandbox::disabled()).is_empty());
        // Even with a real backend discovered and a narrowed network to talk about: `Off` is the
        // absence of a posture, and absence is what gets forwarded.
        let off_with_backend = pinned(
            SandboxMode::Off,
            false,
            &["/output"],
            Backend::Bubblewrap {
                bwrap: PathBuf::from("/usr/bin/bwrap"),
            },
        );
        assert!(posture_env(&off_with_backend).is_empty());
    }

    /// Floor-never-ceiling, second consequence: `FLUX_SANDBOX_NET` is emitted only to say *closed*.
    /// A truthy value beats both `[sandbox] network` and C-262's unattended-closed default on the
    /// reading side, so forwarding "open" would let a parent re-open a network the child would have
    /// shut — a ceiling. An unrestricted network is the absence of a restriction, and absence is
    /// not a parent's to impose.
    #[test]
    fn an_open_network_forwards_nothing_because_only_a_narrowing_is_a_floor() {
        let open = pinned(
            SandboxMode::Require,
            true,
            &[],
            Backend::Bubblewrap {
                bwrap: PathBuf::from("/usr/bin/bwrap"),
            },
        );
        let forwarded = posture_env(&open);
        assert!(
            !forwarded.iter().any(|(k, _)| *k == "FLUX_SANDBOX_NET"),
            "an open network must not be forwarded: {forwarded:?}"
        );
    }

    // -- SpawnPolicy::for_workspace --------------------------------------------------------------

    #[test]
    fn for_workspace_includes_root_named_roots_tmp_and_toolchain_caches() {
        let _g = EnvGuard::new(&["CARGO_HOME", "RUSTUP_HOME", "TMPDIR"]);
        std::env::set_var("CARGO_HOME", "/cargo-home");
        std::env::set_var("RUSTUP_HOME", "/rustup-home");
        std::env::remove_var("TMPDIR");

        let (dir, mut ws) = temp_workspace();
        let named_dir = dir.join("named");
        std::fs::create_dir_all(&named_dir).unwrap();
        ws.add_named_root("extra", &named_dir).unwrap();

        let settings = SandboxSettings {
            mode: SandboxMode::On,
            network: false,
            extra_writable: vec![PathBuf::from("/opt/extra")],
        };
        let policy = workspace_policy(&ws, &settings);

        assert_eq!(policy.cwd, ws.root());
        assert!(!policy.network);
        assert!(policy.writable.contains(&ws.root().to_path_buf()));
        assert!(policy.writable.contains(&named_dir.canonicalize().unwrap()));
        assert!(policy.writable.contains(&PathBuf::from("/tmp")));
        assert!(policy.writable.contains(&PathBuf::from("/cargo-home")));
        assert!(policy.writable.contains(&PathBuf::from("/rustup-home")));
        assert!(policy.writable.contains(&PathBuf::from("/opt/extra")));
    }

    #[test]
    fn for_workspace_defaults_toolchain_caches_under_home() {
        let _g = EnvGuard::new(&["CARGO_HOME", "RUSTUP_HOME"]);
        std::env::remove_var("CARGO_HOME");
        std::env::remove_var("RUSTUP_HOME");
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();

        let (_dir, ws) = temp_workspace();
        let policy = workspace_policy(&ws, &SandboxSettings::off());
        assert!(policy.writable.contains(&home.join(".cargo")));
        assert!(policy.writable.contains(&home.join(".rustup")));
    }

    /// FIX I: an empty `CARGO_HOME`/`RUSTUP_HOME` (or an unset `HOME` yielding a relative
    /// `.cargo`/`.rustup`) must never leak an empty or relative writable entry — those become a
    /// nonsensical `--bind-try "" ""` (bwrap) or a profile-corrupting empty `(subpath (param
    /// "Wn"))` (Seatbelt). No writable path may be empty, and all must be absolute.
    #[test]
    fn for_workspace_skips_empty_or_relative_toolchain_paths() {
        let _g = EnvGuard::new(&["CARGO_HOME", "RUSTUP_HOME", "TMPDIR"]);
        std::env::set_var("CARGO_HOME", "");
        std::env::set_var("RUSTUP_HOME", "");
        std::env::remove_var("TMPDIR");

        let (_dir, ws) = temp_workspace();
        let settings = SandboxSettings {
            mode: SandboxMode::On,
            network: true,
            // A relative extra-writable is also a misconfiguration and must be dropped.
            extra_writable: vec![PathBuf::from(""), PathBuf::from("relative/dir")],
        };
        let policy = workspace_policy(&ws, &settings);

        assert!(
            !policy.writable.iter().any(|p| p.as_os_str().is_empty()),
            "no empty writable entry: {:?}",
            policy.writable
        );
        assert!(
            policy.writable.iter().all(|p| p.is_absolute()),
            "every writable entry must be absolute: {:?}",
            policy.writable
        );
        assert!(
            !policy.writable.contains(&PathBuf::from("relative/dir")),
            "a relative extra-writable must be dropped: {:?}",
            policy.writable
        );
    }

    #[test]
    fn for_workspace_includes_linked_worktree_admin_and_common_dirs() {
        let _g = EnvGuard::new(&["CARGO_HOME", "RUSTUP_HOME", "TMPDIR"]);
        std::env::set_var("CARGO_HOME", "/cargo-home");
        std::env::set_var("RUSTUP_HOME", "/rustup-home");
        std::env::remove_var("TMPDIR");

        let root = temp_dir("linked-worktree");
        let main_git = root.join("main/.git");
        let admin = main_git.join("worktrees/linked");
        let worktree = root.join("linked");
        std::fs::create_dir_all(&admin).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", admin.display()),
        )
        .unwrap();
        std::fs::write(
            admin.join("gitdir"),
            format!("{}\n", worktree.join(".git").display()),
        )
        .unwrap();
        std::fs::write(admin.join("commondir"), "../..\n").unwrap();

        let ws = Workspace::new(&worktree).unwrap();
        let policy = workspace_policy(&ws, &SandboxSettings::off());
        assert!(
            policy.writable.contains(&admin.canonicalize().unwrap()),
            "linked-worktree admin dir must be writable: {:?}",
            policy.writable
        );
        assert!(
            policy.writable.contains(&main_git.canonicalize().unwrap()),
            "linked-worktree common dir must be writable for refs/objects: {:?}",
            policy.writable
        );
    }

    #[test]
    fn for_workspace_does_not_trust_an_arbitrary_gitdir_pointer() {
        let _g = EnvGuard::new(&["CARGO_HOME", "RUSTUP_HOME", "TMPDIR"]);
        std::env::set_var("CARGO_HOME", "/cargo-home");
        std::env::set_var("RUSTUP_HOME", "/rustup-home");
        std::env::remove_var("TMPDIR");

        let root = temp_dir("untrusted-gitdir");
        let worktree = root.join("worktree");
        let arbitrary = root.join("sensitive/worktrees/forged");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::create_dir_all(&arbitrary).unwrap();
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", arbitrary.display()),
        )
        .unwrap();
        std::fs::write(arbitrary.join("commondir"), "../..\n").unwrap();

        let ws = Workspace::new(&worktree).unwrap();
        let policy = workspace_policy(&ws, &SandboxSettings::off());
        assert!(
            !policy.writable.contains(&arbitrary.canonicalize().unwrap()),
            "a workspace-writable .git file must not grant arbitrary host writes"
        );
    }

    // -- test helpers (D-131/D-132) -------------------------------------------------------------

    fn temp_dir(prefix: &str) -> PathBuf {
        fixture_dir(&format!("sandbox-{prefix}"))
    }

    #[cfg(unix)]
    fn write_script(path: &Path, contents: &str) {
        std::fs::write(path, contents).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }

    fn policy_for(
        cwd: &Path,
        writable: Vec<PathBuf>,
        network: bool,
        unconfined: bool,
    ) -> SpawnPolicy {
        SpawnPolicy {
            writable,
            configured_writable: Vec::new(),
            network,
            cwd: cwd.to_path_buf(),
            unconfined,
        }
    }

    // -- bubblewrap_argv: golden argv (D-131) ---------------------------------------------------

    #[test]
    fn bubblewrap_argv_baseline_network_on() {
        let (_dir, ws) = temp_workspace();
        let cwd = ws.root().to_path_buf();
        let cwd_s = cwd.to_string_lossy().into_owned();
        let policy = policy_for(&cwd, vec![cwd.clone()], true, false);
        let argv = vec!["echo".to_string(), "hi".to_string()];

        let out = bubblewrap_argv(Path::new("/usr/bin/bwrap"), &argv, &policy);

        let expected: Vec<String> = [
            "/usr/bin/bwrap",
            "--die-with-parent",
            "--unshare-pid",
            "--unshare-ipc",
            "--unshare-uts",
            "--unshare-cgroup-try",
            "--ro-bind",
            "/",
            "/",
            "--dev",
            "/dev",
            "--proc",
            "/proc",
            "--tmpfs",
            "/run",
            "--dir",
            "/run/systemd",
            "--dir",
            "/run/systemd/resolve",
            "--dir",
            "/run/resolvconf",
            "--dir",
            "/run/NetworkManager",
            "--ro-bind-try",
            "/run/systemd/resolve/resolv.conf",
            "/run/systemd/resolve/resolv.conf",
            "--ro-bind-try",
            "/run/systemd/resolve/stub-resolv.conf",
            "/run/systemd/resolve/stub-resolv.conf",
            "--ro-bind-try",
            "/run/resolvconf/resolv.conf",
            "/run/resolvconf/resolv.conf",
            "--ro-bind-try",
            "/run/NetworkManager/resolv.conf",
            "/run/NetworkManager/resolv.conf",
            "--ro-bind-try",
            "/run/NetworkManager/no-stub-resolv.conf",
            "/run/NetworkManager/no-stub-resolv.conf",
            "--bind",
            "/tmp",
            "/tmp",
            "--bind",
            &cwd_s,
            &cwd_s,
            "--chdir",
            &cwd_s,
            "--",
            "echo",
            "hi",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        assert_eq!(out, expected);
    }

    #[test]
    fn bubblewrap_argv_network_off_adds_unshare_net_and_skips_resolv_rebind() {
        let (_dir, ws) = temp_workspace();
        let cwd = ws.root().to_path_buf();
        let policy = policy_for(&cwd, vec![cwd.clone()], false, false);
        let out = bubblewrap_argv(Path::new("/usr/bin/bwrap"), &[], &policy);

        assert!(out.contains(&"--unshare-net".to_string()));
        // FIX E: none of the resolver state dirs get re-bound when the network is closed.
        for resolver in [
            "/run/systemd/resolve/resolv.conf",
            "/run/systemd/resolve/stub-resolv.conf",
            "/run/resolvconf/resolv.conf",
            "/run/NetworkManager/resolv.conf",
            "/run/NetworkManager/no-stub-resolv.conf",
            "/run/dbus",
        ] {
            assert!(
                !out.iter().any(|a| a == resolver),
                "resolv rebind {resolver} must not appear when network=off: {out:?}"
            );
        }
    }

    #[test]
    fn bubblewrap_argv_network_on_restores_dns_without_host_ipc_sockets() {
        let (_dir, ws) = temp_workspace();
        let cwd = ws.root().to_path_buf();
        let policy = policy_for(&cwd, vec![cwd.clone()], true, false);
        let out = bubblewrap_argv(Path::new("/usr/bin/bwrap"), &[], &policy);

        // FIX E: /etc/resolv.conf symlinks into /run on non-systemd-resolved distros too, so every
        // common resolver state dir is re-exposed after the /run tmpfs mask when network=on.
        for resolver in [
            "/run/systemd/resolve/resolv.conf",
            "/run/systemd/resolve/stub-resolv.conf",
            "/run/resolvconf/resolv.conf",
            "/run/NetworkManager/resolv.conf",
            "/run/NetworkManager/no-stub-resolv.conf",
        ] {
            assert!(
                windowed_contains(&out, &["--ro-bind-try", resolver, resolver]),
                "expected `--ro-bind-try {resolver} {resolver}` when network=on: {out:?}"
            );
        }
        for masked in ["/run/dbus", "/run/NetworkManager", "/run/systemd/resolve"] {
            assert!(
                !windowed_contains(&out, &["--ro-bind-try", masked, masked]),
                "host IPC directory {masked} must stay hidden behind the /run tmpfs: {out:?}"
            );
        }
    }

    #[test]
    fn wrap_argv_rejects_root_as_an_extra_writable_mount() {
        let sandbox = Sandbox {
            settings: SandboxSettings {
                mode: SandboxMode::On,
                network: true,
                extra_writable: vec![PathBuf::from("/")],
            },
            backend: Backend::Bubblewrap {
                bwrap: PathBuf::from("/usr/bin/bwrap"),
            },
        };
        let (_dir, ws) = temp_workspace();
        let policy = workspace_policy(&ws, sandbox.settings());
        let err = sandbox
            .wrap_argv(&["true".to_string()], &policy)
            .expect_err("a late `--bind / /` would erase the special mounts");
        assert!(err.to_string().contains("writable root"), "{err}");
    }

    #[test]
    fn wrap_argv_rejects_root_from_automatic_tmpdir_too() {
        let (_dir, ws) = temp_workspace();
        let _g = EnvGuard::new(&["TMPDIR"]);
        std::env::set_var("TMPDIR", "/");
        let sandbox = Sandbox {
            settings: SandboxSettings {
                mode: SandboxMode::On,
                network: true,
                extra_writable: Vec::new(),
            },
            backend: Backend::Bubblewrap {
                bwrap: PathBuf::from("/usr/bin/bwrap"),
            },
        };
        let policy = workspace_policy(&ws, sandbox.settings());
        assert!(policy.writable.contains(&PathBuf::from("/")));
        assert!(sandbox.wrap_argv(&["true".to_string()], &policy).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn wrap_argv_rejects_a_writable_symlink_that_resolves_to_root() {
        let (dir, ws) = temp_workspace();
        let root_link = dir.join("root-link");
        std::os::unix::fs::symlink("/", &root_link).unwrap();
        let sandbox = Sandbox {
            settings: SandboxSettings {
                mode: SandboxMode::On,
                network: true,
                extra_writable: vec![root_link],
            },
            backend: Backend::Bubblewrap {
                bwrap: PathBuf::from("/usr/bin/bwrap"),
            },
        };
        let policy = workspace_policy(&ws, sandbox.settings());
        assert!(sandbox.wrap_argv(&["true".to_string()], &policy).is_err());
    }

    #[test]
    fn wrap_argv_creates_configured_writable_dirs_and_uses_a_required_bind() {
        let root = temp_dir("create-configured-writable");
        let worktree = root.join("worktree");
        let output = root.join("new/output");
        std::fs::create_dir_all(&worktree).unwrap();
        assert!(!output.exists());
        let sandbox = Sandbox {
            settings: SandboxSettings {
                mode: SandboxMode::On,
                network: true,
                extra_writable: vec![output.clone()],
            },
            backend: Backend::Bubblewrap {
                bwrap: PathBuf::from("/usr/bin/bwrap"),
            },
        };
        let ws = Workspace::new(&worktree).unwrap();
        let policy = workspace_policy(&ws, sandbox.settings());
        let out = sandbox.wrap_argv(&["true".to_string()], &policy).unwrap();
        assert!(
            output.is_dir(),
            "configured output root should be created before bwrap"
        );
        let output = output.to_string_lossy().into_owned();
        assert!(
            windowed_contains(&out, &["--bind", &output, &output]),
            "{out:?}"
        );
        assert!(
            !windowed_contains(&out, &["--bind-try", &output, &output]),
            "configured paths must fail loudly if they disappear before exec: {out:?}"
        );
    }

    #[test]
    fn bubblewrap_argv_includes_extra_writable_binds() {
        let (dir, ws) = temp_workspace();
        let cwd = ws.root().to_path_buf();
        let extra = dir.join("extra-writable");
        let policy = policy_for(&cwd, vec![cwd.clone(), extra.clone()], true, false);
        let out = bubblewrap_argv(Path::new("/usr/bin/bwrap"), &[], &policy);

        let extra_s = extra.to_string_lossy().into_owned();
        // The workspace root gets a real `--bind` (must exist); the extra gets `--bind-try` (may
        // not exist).
        let cwd_s = cwd.to_string_lossy().into_owned();
        assert!(windowed_contains(&out, &["--bind", &cwd_s, &cwd_s]));
        assert!(windowed_contains(&out, &["--bind-try", &extra_s, &extra_s]));
    }

    #[test]
    fn bubblewrap_argv_unconfined_collapses_fs_binds_but_keeps_lifecycle_network_and_run_masking() {
        let (_dir, ws) = temp_workspace();
        let cwd = ws.root().to_path_buf();
        let policy = policy_for(&cwd, vec![cwd.clone()], false, true);
        let out = bubblewrap_argv(Path::new("/usr/bin/bwrap"), &["true".to_string()], &policy);

        // fs binds collapse to a single `--bind / /` — no `--ro-bind / /`, no per-path binds.
        assert!(windowed_contains(&out, &["--bind", "/", "/"]));
        assert!(!windowed_contains(&out, &["--ro-bind", "/", "/"]));
        assert!(
            !out.iter().any(|a| a == "/tmp"),
            "no explicit /tmp bind under unconfined: {out:?}"
        );
        let cwd_s = cwd.to_string_lossy().into_owned();
        assert!(
            !windowed_contains(&out, &["--bind", &cwd_s, &cwd_s]),
            "no explicit workspace bind under unconfined (already covered by --bind / /): {out:?}"
        );
        // Lifecycle, network, and /run masking are unaffected by `unconfined`.
        assert!(out.contains(&"--die-with-parent".to_string()));
        assert!(out.contains(&"--unshare-net".to_string()));
        assert!(windowed_contains(&out, &["--tmpfs", "/run"]));
    }

    /// True if `needle` appears as a contiguous subsequence of `haystack` (order-sensitive,
    /// position-agnostic) — used for assertions about argv fragments without pinning exact indices.
    fn windowed_contains(haystack: &[String], needle: &[&str]) -> bool {
        if needle.is_empty() || haystack.len() < needle.len() {
            return false;
        }
        haystack
            .windows(needle.len())
            .any(|w| w.iter().map(String::as_str).eq(needle.iter().copied()))
    }

    // -- discovery: absolute-path invariant (D-131) ---------------------------------------------

    #[cfg(target_os = "linux")]
    #[test]
    fn discover_bwrap_via_path_returns_absolute_path_never_a_bare_name() {
        let dir = temp_dir("bwrap-path");
        write_script(&dir.join("bwrap"), "#!/bin/sh\nexit 0\n");

        let found = discover_bwrap_in(None, Some(dir.as_os_str()))
            .expect("fake bwrap discoverable on injected PATH");
        assert!(found.is_absolute(), "must be absolute: {found:?}");
        assert_ne!(found, PathBuf::from("bwrap"), "must never be the bare name");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn discover_bwrap_env_override_wins_and_is_absolutized() {
        let dir = temp_dir("bwrap-override");
        let custom = dir.join("custom-bwrap");
        write_script(&custom, "#!/bin/sh\nexit 0\n");

        let found = discover_bwrap_in(Some(custom.as_os_str()), None).unwrap();
        assert_eq!(found, custom.canonicalize().unwrap());
        assert!(found.is_absolute());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn discover_bwrap_missing_names_flux_bwrap_bin_in_the_reason() {
        let dir = temp_dir("bwrap-empty-path");
        let err = discover_bwrap_in(None, Some(dir.as_os_str())).unwrap_err();
        assert!(err.contains("not found on PATH"), "{err}");
        assert!(err.contains("FLUX_BWRAP_BIN"), "{err}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn resolve_uses_the_callers_path_to_find_an_absolute_probe_command() {
        let dir = temp_dir("bwrap-non-fhs-probe");
        write_script(
            &dir.join("bwrap"),
            "#!/bin/sh\nlast=\nfor arg in \"$@\"; do last=$arg; done\ncase \"$last\" in /*) exit 0;; *) echo 'probe command was not absolute' >&2; exit 9;; esac\n",
        );
        write_script(&dir.join("true"), "#!/bin/sh\nexit 0\n");

        let path = dir.as_os_str();
        let bwrap = discover_bwrap_in(None, Some(path)).unwrap();
        let command = discover_probe_executable_in("true", Some(path)).unwrap();
        assert!(
            command.is_absolute(),
            "probe command must be pinned: {command:?}"
        );
        assert_eq!(
            run_probe(&bwrap, &bwrap_probe_argv(&command), Duration::from_secs(2)),
            ProbeOutcome::Ok
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn injected_discovery_never_mutates_the_process_path() {
        let original = std::env::var_os("PATH");
        let dir = temp_dir("bwrap-pure-path");
        write_script(&dir.join("bwrap"), "#!/bin/sh\nexit 0\n");
        let _ = discover_bwrap_in(None, Some(dir.as_os_str())).unwrap();
        assert_eq!(std::env::var_os("PATH"), original);
    }

    // -- preflight probe classification (D-131) -------------------------------------------------

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn run_probe_classifies_ok() {
        let dir = temp_dir("probe-ok");
        let script = dir.join("ok.sh");
        write_script(&script, "#!/bin/sh\nexit 0\n");
        assert_eq!(
            run_probe(&script, &[], Duration::from_secs(2)),
            ProbeOutcome::Ok
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn run_probe_classifies_missing_when_the_binary_does_not_exist() {
        let dir = temp_dir("probe-missing");
        let missing = dir.join("does-not-exist");
        assert!(matches!(
            run_probe(&missing, &[], Duration::from_secs(2)),
            ProbeOutcome::Missing(_)
        ));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn run_probe_classifies_namespaces_denied_from_stderr_patterns() {
        let dir = temp_dir("probe-denied");
        for (name, stderr_line) in [
            ("denied1.sh", "Operation not permitted"),
            (
                "denied2.sh",
                "bwrap: Creating new namespace failed: Operation not permitted",
            ),
            ("denied3.sh", "bwrap: setting up uid map: Permission denied"),
            ("denied4.sh", "No permissions to create new namespace"),
        ] {
            let script = dir.join(name);
            write_script(
                &script,
                &format!("#!/bin/sh\necho '{stderr_line}' >&2\nexit 1\n"),
            );
            assert_eq!(
                run_probe(&script, &[], Duration::from_secs(2)),
                ProbeOutcome::NamespacesDenied,
                "{name}"
            );
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn run_probe_classifies_broken_for_an_unrelated_nonzero_exit() {
        let dir = temp_dir("probe-broken");
        let script = dir.join("broken.sh");
        write_script(
            &script,
            "#!/bin/sh\necho 'something else went wrong' >&2\nexit 3\n",
        );
        match run_probe(&script, &[], Duration::from_secs(2)) {
            ProbeOutcome::Broken(stderr) => {
                assert!(stderr.contains("something else went wrong"), "{stderr}")
            }
            other => panic!("expected Broken, got {other:?}"),
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn run_probe_kills_forked_stderr_holders_without_waiting_for_pipe_eof() {
        let dir = temp_dir("probe-forked-stderr");
        let pid_file = dir.join("descendant.pid");
        let script = dir.join("fork.sh");
        write_script(
            &script,
            &format!(
                "#!/bin/sh\nsleep 30 &\necho $! > '{}'\necho 'wrapper failed' >&2\nexit 7\n",
                pid_file.display()
            ),
        );

        let started = std::time::Instant::now();
        let outcome = run_probe(&script, &[], Duration::from_millis(500));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "probe blocked: {outcome:?}"
        );
        assert!(
            matches!(outcome, ProbeOutcome::Broken(ref stderr) if stderr.contains("wrapper failed")),
            "{outcome:?}"
        );

        let pid: libc::pid_t = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        for _ in 0..20 {
            // SAFETY: signal 0 performs an existence check only; `pid` came from the test-owned
            // descendant and is positive.
            if unsafe { libc::kill(pid, 0) } != 0 {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("probe descendant {pid} survived guarded process-group cleanup");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn run_probe_caps_stderr_while_continuing_to_drain() {
        let dir = temp_dir("probe-capped-stderr");
        let script = dir.join("loud.sh");
        // Emit 2 MiB of stderr with shell builtins only (`for`/assignment/`printf`): the earlier
        // `head`/`tr` pipeline depended on those binaries resolving under the probe's scrubbed
        // env, which fails on minimal-PATH CI runners ("head: not found"). 16 * 2^17 = 2097152.
        write_script(
            &script,
            "#!/bin/sh\ns=xxxxxxxxxxxxxxxx\nfor _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17; do s=\"$s$s\"; done\nprintf '%s' \"$s\" >&2\nexit 4\n",
        );
        match run_probe(&script, &[], Duration::from_secs(2)) {
            ProbeOutcome::Broken(stderr) => {
                assert!(stderr.len() < 2 * 1024 * 1024, "stderr was not bounded");
                assert!(stderr.contains("output truncated"), "{stderr:?}");
            }
            other => panic!("expected Broken, got {other:?}"),
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn probe_cached_caches_by_binary_path() {
        let dir = temp_dir("probe-cache");
        let script = dir.join("flip.sh");
        write_script(&script, "#!/bin/sh\nexit 0\n");
        assert_eq!(probe_cached(&script, &[]), ProbeOutcome::Ok);

        // Mutate the script to now fail — the cached outcome must not change (no re-probe).
        write_script(&script, "#!/bin/sh\nexit 7\n");
        assert_eq!(
            probe_cached(&script, &[]),
            ProbeOutcome::Ok,
            "cached outcome must survive a change to the underlying binary"
        );
    }

    // -- Sandbox::resolve folds discovery + probe together (D-131) ------------------------------

    #[test]
    fn resolve_with_mode_off_skips_discovery_entirely() {
        let _g = EnvGuard::new(&["FLUX_SANDBOXED", "FLUX_SANDBOX"]);
        std::env::remove_var("FLUX_SANDBOXED");
        let sandbox = Sandbox::resolve(SandboxSettings::off());
        assert_eq!(sandbox.reason(), Some("sandbox disabled"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn resolve_activates_bubblewrap_when_the_probe_succeeds() {
        let dir = temp_dir("resolve-ok");
        let fake = dir.join("bwrap");
        write_script(&fake, "#!/bin/sh\nexit 0\n");

        let _g = EnvGuard::new(&["FLUX_SANDBOXED", "FLUX_BWRAP_BIN"]);
        std::env::remove_var("FLUX_SANDBOXED");
        std::env::set_var("FLUX_BWRAP_BIN", &fake);

        let settings = SandboxSettings {
            mode: SandboxMode::On,
            network: true,
            extra_writable: Vec::new(),
        };
        let sandbox = Sandbox::resolve(settings);
        assert!(sandbox.is_active(), "reason: {:?}", sandbox.reason());
        assert_eq!(sandbox.describe(), "sandbox: active (bubblewrap)");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn resolve_auto_degrades_under_on_when_namespaces_denied() {
        let dir = temp_dir("resolve-denied-on");
        let fake = dir.join("bwrap");
        write_script(
            &fake,
            "#!/bin/sh\necho 'Creating new namespace failed: Operation not permitted' >&2\nexit 1\n",
        );

        let _g = EnvGuard::new(&["FLUX_SANDBOXED", "FLUX_BWRAP_BIN"]);
        std::env::remove_var("FLUX_SANDBOXED");
        std::env::set_var("FLUX_BWRAP_BIN", &fake);

        let settings = SandboxSettings {
            mode: SandboxMode::On,
            network: true,
            extra_writable: Vec::new(),
        };
        let sandbox = Sandbox::resolve(settings);
        assert!(
            !sandbox.is_active(),
            "must auto-degrade under `on` when namespaces are denied (Docker/hardened kernels)"
        );
        let reason = sandbox.reason().expect("names why");
        assert!(reason.to_lowercase().contains("namespace"), "{reason}");
        assert!(
            sandbox.ensure_available().is_ok(),
            "`on` mode is a soft degrade, not a hard failure"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn resolve_fails_closed_under_require_when_namespaces_denied() {
        let dir = temp_dir("resolve-denied-require");
        let fake = dir.join("bwrap");
        write_script(
            &fake,
            "#!/bin/sh\necho 'Creating new namespace failed: Operation not permitted' >&2\nexit 1\n",
        );

        let _g = EnvGuard::new(&["FLUX_SANDBOXED", "FLUX_BWRAP_BIN"]);
        std::env::remove_var("FLUX_SANDBOXED");
        std::env::set_var("FLUX_BWRAP_BIN", &fake);

        let settings = SandboxSettings {
            mode: SandboxMode::Require,
            network: true,
            extra_writable: Vec::new(),
        };
        let sandbox = Sandbox::resolve(settings);
        assert!(!sandbox.is_active());
        assert!(sandbox.ensure_available().is_err());
    }

    // -- Sandbox::wrap_argv dispatch (D-131/D-132) -----------------------------------------------

    #[test]
    fn wrap_argv_dispatches_to_bubblewrap_when_active() {
        let sandbox = Sandbox {
            settings: SandboxSettings {
                mode: SandboxMode::On,
                network: true,
                extra_writable: Vec::new(),
            },
            backend: Backend::Bubblewrap {
                bwrap: PathBuf::from("/usr/bin/bwrap"),
            },
        };
        let (_dir, ws) = temp_workspace();
        let policy = workspace_policy(&ws, sandbox.settings());
        let out = sandbox.wrap_argv(&["true".to_string()], &policy).unwrap();
        assert_eq!(out[0], "/usr/bin/bwrap");
        assert_eq!(out.last().unwrap(), "true");
    }

    #[test]
    fn wrap_argv_dispatches_to_seatbelt_when_active() {
        let sandbox = Sandbox {
            settings: SandboxSettings {
                mode: SandboxMode::On,
                network: true,
                extra_writable: Vec::new(),
            },
            backend: Backend::Seatbelt {
                sandbox_exec: PathBuf::from("/usr/bin/sandbox-exec"),
            },
        };
        let (_dir, ws) = temp_workspace();
        let policy = workspace_policy(&ws, sandbox.settings());
        let out = sandbox.wrap_argv(&["true".to_string()], &policy).unwrap();
        assert_eq!(out[0], "/usr/bin/sandbox-exec");
        assert_eq!(out.last().unwrap(), "true");
    }

    #[test]
    fn wrap_argv_rejects_unsafe_seatbelt_paths_before_building_the_profile() {
        let sandbox = Sandbox {
            settings: SandboxSettings {
                mode: SandboxMode::On,
                network: true,
                extra_writable: Vec::new(),
            },
            backend: Backend::Seatbelt {
                sandbox_exec: PathBuf::from("/usr/bin/sandbox-exec"),
            },
        };
        let bad = PathBuf::from("/tmp/\"evil\"");
        let policy = policy_for(&bad, vec![bad.clone()], true, false);
        let err = sandbox
            .wrap_argv(&["true".to_string()], &policy)
            .unwrap_err();
        assert!(err.to_string().contains("unsafe"), "{err}");
    }

    // -- seatbelt_argv / seatbelt_profile: golden profile (D-132, cfg-free) ----------------------

    #[test]
    fn seatbelt_argv_baseline_network_on() {
        let (_dir, ws) = temp_workspace();
        let cwd = ws.root().to_path_buf();
        let policy = policy_for(&cwd, vec![cwd.clone()], true, false);
        let argv = vec!["echo".to_string(), "hi".to_string()];

        let out = seatbelt_argv(Path::new("/usr/bin/sandbox-exec"), &argv, &policy);

        assert_eq!(out[0], "/usr/bin/sandbox-exec");
        assert_eq!(out[1], "-D");
        assert_eq!(out[2], format!("WS_ROOT={}", cwd.display()));
        assert_eq!(out[3], "-D");
        assert!(out[4].starts_with("TMP="), "{:?}", out[4]);
        assert_eq!(out[5], "-p");
        let profile = &out[6];
        assert!(profile.starts_with("(version 1)(allow default)"));
        assert!(profile.contains("(deny file-write*)"));
        assert!(profile.contains("(subpath (param \"WS_ROOT\"))"));
        assert!(profile.contains("(subpath (param \"TMP\"))"));
        assert!(profile.contains("\"/private/tmp\""));
        assert!(profile.contains("\"/private/var/tmp\""));
        assert!(profile.contains("/dev/null"));
        assert!(profile.contains("^/dev/tty"));
        assert!(profile.contains("^/dev/fd/"));
        assert!(
            !profile.contains("deny network"),
            "network=on must not deny: {profile}"
        );
        assert_eq!(&out[7..], &["echo".to_string(), "hi".to_string()]);
    }

    #[test]
    fn seatbelt_argv_network_off_denies_network() {
        let (_dir, ws) = temp_workspace();
        let cwd = ws.root().to_path_buf();
        let policy = policy_for(&cwd, vec![cwd.clone()], false, false);
        let out = seatbelt_argv(Path::new("/usr/bin/sandbox-exec"), &[], &policy);
        let profile = out.iter().find(|s| s.starts_with("(version 1)")).unwrap();
        assert!(profile.contains("(deny network*)"), "{profile}");
    }

    #[test]
    fn seatbelt_argv_includes_numbered_extras() {
        let (dir, ws) = temp_workspace();
        let cwd = ws.root().to_path_buf();
        let extra = dir.join("extra-writable");
        std::fs::create_dir_all(&extra).unwrap();
        let policy = policy_for(&cwd, vec![cwd.clone(), extra.clone()], true, false);
        let out = seatbelt_argv(Path::new("/usr/bin/sandbox-exec"), &[], &policy);

        let canon = extra.canonicalize().unwrap();
        assert!(out.contains(&format!("W0={}", canon.display())), "{out:?}");
        let profile = out.iter().find(|s| s.starts_with("(version 1)")).unwrap();
        assert!(profile.contains("(subpath (param \"W0\"))"), "{profile}");
    }

    #[test]
    fn seatbelt_argv_unconfined_skips_file_write_block_but_keeps_network_deny() {
        let (_dir, ws) = temp_workspace();
        let cwd = ws.root().to_path_buf();
        let policy = policy_for(&cwd, vec![cwd.clone()], false, true);
        let out = seatbelt_argv(Path::new("/usr/bin/sandbox-exec"), &[], &policy);
        let profile = out.iter().find(|s| s.starts_with("(version 1)")).unwrap();
        assert!(!profile.contains("file-write"), "{profile}");
        assert!(profile.contains("(deny network*)"), "{profile}");
    }

    #[cfg(unix)]
    #[test]
    fn seatbelt_argv_canonicalizes_writable_paths_through_symlinks() {
        let dir = temp_dir("seatbelt-canon");
        let real = dir.join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let (_wsdir, ws) = temp_workspace();
        let cwd = ws.root().to_path_buf();
        let policy = policy_for(&cwd, vec![cwd.clone(), link.clone()], true, false);
        let out = seatbelt_argv(Path::new("/usr/bin/sandbox-exec"), &[], &policy);

        let real_canon = real.canonicalize().unwrap();
        assert!(
            out.contains(&format!("W0={}", real_canon.display())),
            "expected the symlink target, not the symlink itself: {out:?}"
        );
        assert!(
            !out.iter().any(|a| a == &format!("W0={}", link.display())),
            "must not leak the symlink path itself: {out:?}"
        );
    }

    // -- reject_unsafe_seatbelt_paths: escaping rejection (D-132) --------------------------------

    #[test]
    fn reject_unsafe_seatbelt_paths_rejects_embedded_quote() {
        let cwd = PathBuf::from("/tmp/ok");
        let policy = policy_for(&cwd, vec![PathBuf::from("/tmp/\"evil\"")], true, false);
        assert!(reject_unsafe_seatbelt_paths(&policy).is_err());
    }

    #[test]
    fn reject_unsafe_seatbelt_paths_rejects_control_characters() {
        let cwd = PathBuf::from("/tmp/ok");
        let policy = policy_for(&cwd, vec![PathBuf::from("/tmp/evil\nname")], true, false);
        assert!(reject_unsafe_seatbelt_paths(&policy).is_err());
    }

    #[test]
    fn reject_unsafe_seatbelt_paths_accepts_ordinary_paths() {
        let (_dir, ws) = temp_workspace();
        let cwd = ws.root().to_path_buf();
        let policy = policy_for(&cwd, vec![cwd.clone()], true, false);
        assert!(reject_unsafe_seatbelt_paths(&policy).is_ok());
    }
}
