//! The **sandboxed** peer backend: OS confinement as a *selectable substrate*, not only a
//! spawn-time modifier (Decision 0018 rule 3).
//!
//! [`crate::sandbox`] is the modifier: a [`Sandbox`] resolved from `FLUX_SANDBOX`/`[sandbox]` and
//! applied inside the native [`System`]'s single spawn choke point. That path is unchanged and
//! stays the default. What this module adds is the *entity* — a type that **is** an
//! [`ExecutionSystem`](crate::port::ExecutionSystem), so a `[[host]]` binding, a posture floor, or
//! any other selector can name confinement the same way it names `local` or `remote`.
//!
//! # It composes; it does not re-implement
//!
//! [`SandboxedSystem`] holds a native [`System`] whose [`Sandbox`] confines, and every guarded
//! operation is pure delegation to that system's inherent method — the same delegation
//! [`crate::port`]'s native impls are. No second guard, no second spawn path, no widened surface:
//! a `SandboxedSystem` can do exactly what the `System` inside it could do, which is why it is a
//! reviewable entry in `flux-codegate`'s backend census rather than a new IO seam.
//!
//! # Why it fails closed at resolution
//!
//! The modifier's contract is graded — `on` degrades with a disclosure, `require` refuses. A
//! *backend* has no such gradient: something selected it by name, and a substrate that answered
//! "I am the confined one" while running unconfined would be lying to the surface that chose it.
//! So [`SandboxedSystem::resolve`] admits only a [`Sandbox`] that actually confines (an active
//! backend of its own, or an outer flux sandbox already holding the whole tree) and refuses
//! otherwise, with the discovered reason. That refusal is `require` semantics applied at selection
//! time, one step earlier than [`Sandbox::ensure_available`]'s per-spawn backstop.
//!
//! For the same reason [`SandboxedSystem::from_env`] floors the posture at
//! [`SandboxMode::Require`] rather than reading whatever the ambient posture happens to be:
//! confinement is what was selected, and every posture source in flux resolves tightest-wins, so
//! `off` cannot lower a selection made explicitly.

use std::net::SocketAddr;
use std::time::Duration;

use flux_core::{Error, Result};

use crate::net::{
    BindExposure, DatagramEndpoint, DialTarget, InboundLimits, NetworkListener, NetworkStream,
    PrivateNetAllow,
};
use crate::port::{
    ExecutionIdentity, Guarded, GuardedEnv, GuardedHostFiles, GuardedNetwork, GuardedProcess,
    GuardedWorkspaceFiles, SubstrateIdentity,
};
use crate::sandbox::{Sandbox, SandboxMode, SandboxSettings};
use crate::websocket::{GuardedWebSocketSession, WebSocketConnect};
use crate::{ManagedChild, OutputObserver, ProcessOutput, ScopedFileRead, System};

/// The stable substrate kind this backend reports through [`SubstrateIdentity::kind`], and the
/// `[[host]]` `backend` value that selects it. Public so a surface can name the peer without
/// copying the string.
pub const KIND: &str = "sandboxed";

/// A native [`System`] whose spawns are confined by an OS sandbox, presented as a peer execution
/// substrate.
///
/// Construct it through [`resolve`](Self::resolve) or [`from_env`](Self::from_env); both refuse
/// unless the composed [`Sandbox`] genuinely confines, so an existing value is itself the evidence
/// that this substrate is confined.
#[derive(Debug, Clone)]
pub struct SandboxedSystem {
    /// The composed native substrate. Its [`Sandbox`] is the confinement; nothing here re-derives
    /// one.
    inner: System,
}

impl SandboxedSystem {
    /// Compose `system` with an already-resolved `sandbox`, or refuse.
    ///
    /// Admits exactly the two states in which this process's children are really confined:
    /// [`Sandbox::is_active`] (a backend of its own wraps each spawn) and
    /// [`Sandbox::confined_by_parent`] (an outer flux sandbox already holds the whole tree, so this
    /// process adds no wrapper *and needs none*). Everything else — no usable backend on this
    /// platform, a wrapper binary that failed its preflight probe, a disabled posture — is the
    /// refusal face: the binding fails closed and names the discovered reason rather than
    /// degrading to an unconfined substrate.
    ///
    /// Takes the resolved sandbox rather than settings so the decision is the caller's to state and
    /// a test's to supply; [`from_env`](Self::from_env) is the production spelling.
    pub fn resolve(system: System, sandbox: Sandbox) -> Result<Self> {
        if !(sandbox.is_active() || sandbox.confined_by_parent()) {
            let reason = sandbox
                .reason()
                .unwrap_or("no confinement backend was resolved");
            return Err(Error::Config(format!(
                "the `{KIND}` backend needs OS confinement and none is usable here: {reason}. \
                 Selection fails closed — a backend selected by name may not run unconfined. \
                 Install a supported sandbox backend, run flux inside an outer container/VM that \
                 provides equivalent isolation, or select a different host binding."
            )));
        }
        Ok(Self {
            inner: system.with_sandbox(sandbox),
        })
    }

    /// The production constructor: confine `system` using the process's sandbox settings, floored
    /// at [`SandboxMode::Require`].
    ///
    /// An already-confining posture on `system` is reused as-is — re-resolving would pay for a
    /// second backend discovery to reach the same answer. Otherwise the environment's settings
    /// (network policy, extra writable paths) are taken with the mode raised to `Require`, because
    /// naming this backend *is* the requirement; `FLUX_SANDBOX=off` describes the modifier and
    /// never lowers an explicit selection.
    pub fn from_env(system: System) -> Result<Self> {
        let current = system.sandbox();
        let sandbox = if current.is_active() || current.confined_by_parent() {
            current.clone()
        } else {
            Sandbox::resolve(SandboxSettings {
                mode: SandboxMode::Require,
                ..SandboxSettings::from_env()
            })
        };
        Self::resolve(system, sandbox)
    }

    /// The composed native substrate. Read-only: re-rooting or re-posturing produces a different
    /// backend, which has to go back through [`resolve`](Self::resolve) and be admitted again.
    pub fn system(&self) -> &System {
        &self.inner
    }
}

impl ExecutionIdentity for SandboxedSystem {
    /// Reports the composed sandbox's **own** posture verbatim ([`Sandbox::describe`]) rather than
    /// a claim of this type's own, so "active (bubblewrap)" and "confined by parent flux" reach an
    /// operator as the different facts they are. `remotely_reported` is false: everything here is
    /// observed in this process, on this machine.
    fn substrate_identity(&self) -> SubstrateIdentity {
        SubstrateIdentity {
            kind: KIND.into(),
            workspace: self.inner.workspace().root().display().to_string(),
            confinement: self.inner.sandbox().describe(),
            remotely_reported: false,
        }
    }
}

// ---------------------------------------------------------------------------
// The port, by delegation
// ---------------------------------------------------------------------------
//
// Every impl below forwards to the composed `System`'s inherent method — the same shape the native
// impls in `port.rs` take, and for the same reason: the port and the struct must be one code path,
// not two that agree today. Nothing is overridden, filtered or re-guarded here; the confinement is
// entirely the `Sandbox` inside `inner`.

impl GuardedEnv for SandboxedSystem {
    fn env(&self, key: &str) -> Option<String> {
        System::env(&self.inner, key)
    }
}

impl GuardedProcess for SandboxedSystem {
    fn run_with_env<'a>(
        &'a self,
        argv: &'a [String],
        env: &'a [(String, String)],
        timeout: Duration,
    ) -> Guarded<'a, ProcessOutput> {
        Box::pin(System::run_with_env(&self.inner, argv, env, timeout))
    }

    fn run<'a>(&'a self, argv: &'a [String], timeout: Duration) -> Guarded<'a, ProcessOutput> {
        Box::pin(System::run(&self.inner, argv, timeout))
    }

    fn run_with_env_observed<'a>(
        &'a self,
        argv: &'a [String],
        env: &'a [(String, String)],
        timeout: Duration,
        observer: OutputObserver,
    ) -> Guarded<'a, ProcessOutput> {
        Box::pin(System::run_with_env_observed(
            &self.inner,
            argv,
            env,
            timeout,
            observer,
        ))
    }

    fn run_observed<'a>(
        &'a self,
        argv: &'a [String],
        timeout: Duration,
        observer: OutputObserver,
    ) -> Guarded<'a, ProcessOutput> {
        Box::pin(System::run_observed(&self.inner, argv, timeout, observer))
    }

    fn run_with_stdin<'a>(
        &'a self,
        argv: &'a [String],
        stdin: &'a [u8],
        timeout: Duration,
    ) -> Guarded<'a, ProcessOutput> {
        Box::pin(System::run_with_stdin(&self.inner, argv, stdin, timeout))
    }

    fn spawn_background<'a>(
        &'a self,
        argv: &'a [String],
        env: &'a [(String, String)],
    ) -> Guarded<'a, ManagedChild> {
        Box::pin(async move { System::spawn_background(&self.inner, argv, env) })
    }
}

impl GuardedNetwork for SandboxedSystem {
    fn open_websocket_scoped<'a>(
        &'a self,
        connect: &'a WebSocketConnect,
        allow: &'a PrivateNetAllow,
    ) -> Guarded<'a, GuardedWebSocketSession> {
        GuardedNetwork::open_websocket_scoped(&self.inner, connect, allow)
    }

    fn dial_scoped<'a>(
        &'a self,
        target: &'a DialTarget,
        allow: &'a PrivateNetAllow,
    ) -> Guarded<'a, NetworkStream> {
        GuardedNetwork::dial_scoped(&self.inner, target, allow)
    }

    fn bind_tcp<'a>(
        &'a self,
        addr: SocketAddr,
        exposure: BindExposure,
        limits: InboundLimits,
    ) -> Guarded<'a, NetworkListener> {
        GuardedNetwork::bind_tcp(&self.inner, addr, exposure, limits)
    }

    fn bind_udp<'a>(
        &'a self,
        addr: SocketAddr,
        exposure: BindExposure,
        limits: InboundLimits,
        allow: PrivateNetAllow,
    ) -> Guarded<'a, DatagramEndpoint> {
        GuardedNetwork::bind_udp(&self.inner, addr, exposure, limits, allow)
    }
}

impl GuardedHostFiles for SandboxedSystem {
    fn host_path_identity(&self, path: &str) -> Result<String> {
        System::host_path_identity(&self.inner, path)
    }

    fn read_file_scoped<'a>(
        &'a self,
        path: &'a str,
        scope: &'a str,
        max_bytes: usize,
    ) -> Guarded<'a, ScopedFileRead> {
        Box::pin(System::read_file_scoped(
            &self.inner,
            path,
            scope,
            max_bytes,
        ))
    }
}

impl GuardedWorkspaceFiles for SandboxedSystem {
    fn read_file_bytes<'a>(&'a self, path: &'a str) -> Guarded<'a, Vec<u8>> {
        Box::pin(System::read_file_bytes(&self.inner, path))
    }

    fn write_file_bytes<'a>(&'a self, path: &'a str, contents: &'a [u8]) -> Guarded<'a, ()> {
        Box::pin(System::write_file_bytes(&self.inner, path, contents))
    }

    fn read_file<'a>(&'a self, path: &'a str) -> Guarded<'a, String> {
        Box::pin(System::read_file(&self.inner, path))
    }

    fn write_file<'a>(&'a self, path: &'a str, contents: &'a str) -> Guarded<'a, ()> {
        Box::pin(System::write_file(&self.inner, path, contents))
    }

    fn append_file<'a>(&'a self, path: &'a str, contents: &'a str) -> Guarded<'a, ()> {
        Box::pin(System::append_file(&self.inner, path, contents))
    }

    fn read_file_bytes_capped<'a>(
        &'a self,
        path: &'a str,
        max: usize,
    ) -> Guarded<'a, (Vec<u8>, bool)> {
        Box::pin(System::read_file_bytes_capped(&self.inner, path, max))
    }

    fn file_size<'a>(&'a self, path: &'a str) -> Guarded<'a, u64> {
        Box::pin(System::file_size(&self.inner, path))
    }

    fn path_exists<'a>(&'a self, path: &'a str) -> Guarded<'a, bool> {
        Box::pin(System::path_exists(&self.inner, path))
    }

    fn is_dir<'a>(&'a self, path: &'a str) -> Guarded<'a, bool> {
        Box::pin(System::is_dir(&self.inner, path))
    }

    fn file_mtime<'a>(&'a self, path: &'a str) -> Guarded<'a, std::time::SystemTime> {
        Box::pin(System::file_mtime(&self.inner, path))
    }

    fn list_dir<'a>(&'a self, path: &'a str) -> Guarded<'a, Vec<String>> {
        Box::pin(System::list_dir(&self.inner, path))
    }

    fn walk_files<'a>(&'a self, base: &'a str, max: usize) -> Guarded<'a, Vec<String>> {
        Box::pin(System::walk_files(&self.inner, base, max))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::port::ExecutionSystem;
    use crate::sandbox::{fixture_dir, EnvGuard};
    use crate::Workspace;

    /// Every environment key sandbox resolution reads. Cleared for the duration of a test and
    /// restored afterwards, so a test states its whole posture and inherits none of the
    /// developer's.
    const SANDBOX_KEYS: &[&str] = &[
        "FLUX_SANDBOX",
        "FLUX_SANDBOXED",
        "FLUX_SANDBOX_NET",
        "FLUX_SANDBOX_WRITABLE",
        "FLUX_BWRAP_BIN",
    ];

    fn fixture(prefix: &str) -> (std::path::PathBuf, System) {
        let root = fixture_dir(prefix);
        let system = System::new(Workspace::new(&root).unwrap());
        (root, system)
    }

    /// A `Sandbox` that genuinely confines, on every platform and with no wrapper binary: the
    /// marker an outer flux sandbox sets resolves to `Backend::AlreadyConfined`, which is confined
    /// without wrapping anything itself. The alternative — discovering a real bubblewrap/Seatbelt
    /// backend — would make these assertions a property of the machine.
    fn confining_sandbox() -> Sandbox {
        std::env::set_var("FLUX_SANDBOXED", "1");
        let sandbox = Sandbox::resolve(SandboxSettings {
            mode: SandboxMode::Require,
            ..SandboxSettings::off()
        });
        assert!(
            sandbox.confined_by_parent(),
            "the fixture must really be confined, or it proves nothing"
        );
        sandbox
    }

    /// C-651 (acceptance 2) — **the refusal face.** A platform with no usable confinement backend
    /// refuses the binding at resolution rather than handing back an unconfined substrate, and the
    /// refusal carries the discovered reason so an operator can act on it.
    ///
    /// `Sandbox::disabled()` is the platform-independent spelling of "no usable backend": it is
    /// `Unsupported` everywhere, including on a developer machine with a working bubblewrap.
    #[test]
    fn selection_fails_closed_where_no_confinement_backend_is_usable() {
        let _env = EnvGuard::new(SANDBOX_KEYS);
        let (root, system) = fixture("sandboxed-refusal");

        let error = SandboxedSystem::resolve(system, Sandbox::disabled())
            .expect_err("an unconfined sandbox must never produce a `sandboxed` substrate");

        let text = error.to_string();
        assert!(
            text.contains("fails closed"),
            "the refusal must say so in the vocabulary the rest of the surface uses: {text}"
        );
        assert!(
            text.contains("sandbox disabled"),
            "the discovered reason must be surfaced verbatim: {text}"
        );
        assert!(
            text.contains(KIND),
            "the refusal must name the backend that was selected: {text}"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// The same refusal through the production constructor, against a real platform discovery: an
    /// operator-supplied bubblewrap path that is not a usable binary leaves no backend, so
    /// `from_env` refuses instead of falling back to an unconfined native system.
    ///
    /// Linux-only because that is where `FLUX_BWRAP_BIN` is the discovery input; the same shape is
    /// exercised platform-independently by the test above. This is also the case the repository's
    /// `FLUX_BWRAP_BIN=/nonexistent/bwrap` gate variant drives.
    #[cfg(target_os = "linux")]
    #[test]
    fn from_env_fails_closed_when_the_platform_backend_is_unusable() {
        let _env = EnvGuard::new(SANDBOX_KEYS);
        let (root, system) = fixture("sandboxed-refusal-env");
        std::env::set_var("FLUX_BWRAP_BIN", "/nonexistent/bwrap");

        let error = SandboxedSystem::from_env(system)
            .expect_err("an unusable wrapper binary must not resolve a confined substrate");
        let text = error.to_string();
        assert!(text.contains("fails closed"), "{text}");
        assert!(
            text.contains("bwrap"),
            "the discovery failure must reach the operator: {text}"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// C-651 (acceptance 4) — the peer reports its confinement **truthfully**: the string is the
    /// composed sandbox's own `describe()`, not a claim this type invents. Checked against the
    /// native identity of the very same inner system, so the two can only differ where they should
    /// — the kind — and a future refactor that hardcoded a confinement string here would fail.
    #[test]
    fn the_peer_reports_the_composed_sandbox_confinement_not_a_claim_of_its_own() {
        let _env = EnvGuard::new(SANDBOX_KEYS);
        let (root, system) = fixture("sandboxed-identity");

        let peer = SandboxedSystem::resolve(system, confining_sandbox())
            .expect("a confined sandbox resolves the peer");
        let identity = peer.substrate_identity();
        let native = ExecutionIdentity::substrate_identity(peer.system());

        assert_eq!(identity.kind, "sandboxed");
        assert_eq!(native.kind, "native", "the composed system is still native");
        assert!(!identity.remotely_reported, "everything here is observed");
        assert_eq!(
            identity.confinement, native.confinement,
            "the peer must report the composed sandbox's posture verbatim"
        );
        assert_eq!(
            identity.confinement, "sandbox: confined by parent flux",
            "and that posture must be the one the fixture really established"
        );
        assert_eq!(identity.workspace, native.workspace);

        std::fs::remove_dir_all(&root).ok();
    }

    /// C-651 (acceptance 1) — the peer really is an [`ExecutionSystem`], and serving it changes
    /// nothing about the guard: a consumer holding only the erased port is refused the workspace
    /// escapes the concrete `System` refuses, and an in-root path round-trips. Without this the
    /// delegation could quietly lose an override and inherit the trait's denial, which looks like a
    /// working backend right up until something asks.
    #[tokio::test]
    async fn the_peer_serves_the_execution_port_with_the_native_confinement_intact() {
        let _env = EnvGuard::new(SANDBOX_KEYS);
        let (root, system) = fixture("sandboxed-port");
        let outside = fixture_dir("sandboxed-port-outside");
        std::fs::write(outside.join("secret.txt"), "outside").unwrap();
        let escape = format!(
            "../{}/secret.txt",
            outside.file_name().unwrap().to_string_lossy()
        );

        let peer = SandboxedSystem::resolve(system, confining_sandbox()).unwrap();
        let port: &dyn ExecutionSystem = &peer;

        assert!(
            port.read_file(&escape).await.is_err(),
            "the workspace jail must travel with the peer's port"
        );
        assert!(
            port.write_file(&escape, "owned").await.is_err(),
            "the peer must refuse a write out of the workspace"
        );
        assert_eq!(
            std::fs::read_to_string(outside.join("secret.txt")).unwrap(),
            "outside",
            "a refused write through the peer still reached the outside file"
        );

        // Confined, not broken: the optional operations answer through the composed system rather
        // than inheriting the port's fail-closed defaults.
        port.write_file("inside.txt", "kept").await.unwrap();
        assert_eq!(port.read_file("inside.txt").await.unwrap(), "kept");
        assert_eq!(port.file_size("inside.txt").await.unwrap(), 4);
        assert!(port.path_exists("inside.txt").await.unwrap());
        assert_eq!(port.list_dir(".").await.unwrap(), vec!["inside.txt"]);

        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&outside).ok();
    }
}
