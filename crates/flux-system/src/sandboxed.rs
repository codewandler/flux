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
//! operation forwards straight back to that system — to its inherent guarded method where it has
//! one, and to its own [`crate::port`] impl for the families that live there. No second guard, no
//! second spawn path, no widened surface: a `SandboxedSystem` can do exactly what the `System`
//! inside it could do, which is why it is a reviewable entry in `flux-codegate`'s backend census
//! rather than a new IO seam.
//!
//! # Why it fails closed at admission
//!
//! The modifier's contract is graded — `on` degrades with a disclosure, `require` refuses. A
//! *backend* has no such gradient: something selected it by name, and a substrate that answered
//! "I am the confined one" while running unconfined would be lying to the surface that chose it.
//! So admission happens once, at construction, and a value of this type is itself the evidence
//! that this substrate is confined. That is `require` semantics applied at selection time, one
//! step earlier than [`Sandbox::ensure_available`]'s per-spawn backstop.
//!
//! # The marker is not evidence, and this is the subtle part
//!
//! There are two ways a flux process's children are confined. It wraps each spawn itself
//! ([`Sandbox::is_active`]) — a discovered, preflight-probed backend, verified by this process.
//! Or an outer flux sandbox already holds the whole tree ([`Sandbox::confined_by_parent`]), which
//! is **not** verified by this process at all: it is a truthy `FLUX_SANDBOXED` in the ambient
//! environment, and [`Sandbox::wrap_argv`] is identity in that state.
//!
//! [`Sandbox::resolve`] already treats that asymmetry carefully — at [`SandboxMode::Off`] it
//! refuses to re-read the marker, so "a stray `FLUX_SANDBOXED`" cannot make an unconfined run
//! look confined — and the CLI pays for trusting it with a prominent, auditable startup line.
//! Both of those protections are keyed to the *ambient* posture. A peer that re-resolved at
//! `Require` to build itself would step around both: it would revive a marker the ambient posture
//! deliberately left inert, in an invocation whose disclosure already decided not to fire.
//!
//! So [`SandboxedSystem::from_env`] trusts the marker through exactly one channel — an ambient
//! [`Sandbox`] that already concluded it, and therefore already disclosed it. A resolution this
//! type performs for itself admits only a backend it discovered and probed, and
//! [`SandboxedSystem::resolve`] (the primitive, which cannot know where its argument came from)
//! admits only that. A marker reaching either door alone is a refusal that names it, so a stale
//! one can be cleared rather than guessed at.
//!
//! Within that rule confinement is still floored, never lowered: naming this backend *is* a
//! `Require`, and `FLUX_SANDBOX=off` describes the modifier rather than an explicit selection.

use std::net::SocketAddr;
use std::time::Duration;

use flux_core::{Error, Result};

use crate::metrics::{MetricAnswer, MetricKind};
use crate::net::{
    BindExposure, DatagramEndpoint, DialTarget, InboundLimits, NetworkListener, NetworkStream,
    PrivateNetAllow,
};
use crate::port::{
    ExecutionIdentity, Guarded, GuardedEnv, GuardedHostFiles, GuardedHttp, GuardedMetrics,
    GuardedNetwork, GuardedProcess, GuardedWorkspaceFiles, SubstrateIdentity,
};
use crate::sandbox::{Sandbox, SandboxMode, SandboxSettings};
use crate::websocket::{GuardedWebSocketSession, WebSocketConnect};
use crate::{ManagedChild, OutputObserver, ProcessOutput, ScopedFileRead, System};

/// The stable substrate kind this backend reports through [`SubstrateIdentity::kind`], and the
/// `[[host]]` `backend` value that selects it. Public so a surface can name the peer without
/// copying the string.
pub const KIND: &str = "sandboxed";

/// Whether a [`Sandbox::confined_by_parent`] answer counts as evidence at admission.
///
/// It is not a property of the sandbox — the same value means different things depending on who
/// resolved it — so it travels as a separate argument rather than being read off one. See the
/// module documentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OuterConfinement {
    /// The posture was established elsewhere, and whoever established it owed and paid the
    /// disclosure. Reusing that conclusion adds no new trust.
    AlreadyDisclosed,
    /// This type resolved the sandbox itself. A marker here was disclosed by nobody.
    Untrusted,
}

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
    /// Admits **only** [`Sandbox::is_active`]: a backend this process discovered, preflight-probed
    /// and will wrap every spawn with. Nothing else is evidence. In particular a
    /// [`Sandbox::confined_by_parent`] sandbox is refused *here* — see the module documentation
    /// for why a bare `FLUX_SANDBOXED` marker is an assertion rather than a verification, and
    /// [`from_env`](Self::from_env) for the one channel that may trust it.
    ///
    /// Both refusal faces name what they refused: an absent backend carries the discovered reason
    /// (so an operator can install or fix one), and a marker-only sandbox says so explicitly (so a
    /// stale one can be cleared rather than guessed at).
    ///
    /// Takes the resolved sandbox rather than settings so the decision is the caller's to state and
    /// a test's to supply; [`from_env`](Self::from_env) is the production spelling.
    pub fn resolve(system: System, sandbox: Sandbox) -> Result<Self> {
        Self::admit(system, sandbox, OuterConfinement::Untrusted)
    }

    /// The production constructor: confine `system` using the process's sandbox settings, floored
    /// at [`SandboxMode::Require`].
    ///
    /// Two doors, and which one opens is the whole security-relevant decision:
    ///
    /// - `system` already carries a confining [`Sandbox`] — the *ambient* posture reached that
    ///   conclusion at startup. It is reused verbatim, including the outer-confinement case,
    ///   because reaching that conclusion is exactly what makes `flux-cli`'s `apply_sandbox_env`
    ///   emit the auditable "trusting FLUX_SANDBOXED=1" line. Trusting it twice costs nothing new;
    ///   re-resolving would also pay for a second backend discovery to reach the same answer.
    /// - Otherwise this type resolves one for itself, at [`SandboxMode::Require`], and admits only
    ///   a backend that resolution genuinely discovered. A marker surfacing *here* was left inert
    ///   by the ambient posture and disclosed by nobody, so it fails closed.
    pub fn from_env(system: System) -> Result<Self> {
        let ambient = system.sandbox();
        if ambient.is_active() || ambient.confined_by_parent() {
            let ambient = ambient.clone();
            return Self::admit(system, ambient, OuterConfinement::AlreadyDisclosed);
        }
        let resolved = Sandbox::resolve(SandboxSettings {
            mode: SandboxMode::Require,
            ..SandboxSettings::from_env()
        });
        Self::admit(system, resolved, OuterConfinement::Untrusted)
    }

    /// The one admission gate. `outer` says whether a [`Sandbox::confined_by_parent`] answer may
    /// count as evidence, which is true only for a posture something else already established and
    /// disclosed.
    fn admit(system: System, sandbox: Sandbox, outer: OuterConfinement) -> Result<Self> {
        let confined = sandbox.is_active()
            || (sandbox.confined_by_parent() && outer == OuterConfinement::AlreadyDisclosed);
        if !confined {
            return Err(Error::Config(Self::refusal(&sandbox)));
        }
        Ok(Self {
            inner: system.with_sandbox(sandbox),
        })
    }

    /// The refusal text for a sandbox that did not earn admission — two distinct faces, because
    /// "this machine cannot confine" and "this marker is not evidence" need different fixes.
    fn refusal(sandbox: &Sandbox) -> String {
        if sandbox.confined_by_parent() {
            return format!(
                "the `{KIND}` backend will not admit a bare `{marker}` marker: it is an assertion \
                 from the parent environment that this process cannot verify, and the sandbox \
                 posture that would have disclosed it is not in effect here. Selection fails \
                 closed. Clear a stale `{marker}`, or run under a posture that establishes \
                 confinement (`FLUX_SANDBOX=require`, `[sandbox] require`, or an unattended \
                 profile) so the trust in it is stated and audited.",
                marker = crate::sandbox::MARKER_ENV
            );
        }
        let reason = sandbox
            .reason()
            .unwrap_or("no confinement backend was resolved");
        format!(
            "the `{KIND}` backend needs OS confinement and none is usable here: {reason}. \
             Selection fails closed — a backend selected by name may not run unconfined. \
             Install a supported sandbox backend, run flux inside an outer container/VM that \
             provides equivalent isolation, or select a different host binding."
        )
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

/// HTTP delegates like everything else (C-675), and the delegation is the whole implementation.
///
/// C-652 left this impl empty because the peer serves by delegating to a [`System`] and a bare
/// `System` served no HTTP: the native client is `flux_web::NativeHttp` at L5, which this L2 type
/// may not reach, and building one here would be a second egress path beside the reviewed broker —
/// exactly what the codegate `Http` census exists to prevent. That reasoning is unchanged, and
/// nothing here reaches upward.
///
/// What changed is the composed system. A composition site that holds the client can attach it
/// ([`System::with_http`]), so the peer forwards this family the same way it forwards network and
/// metrics — one call, to the system it already holds — and the answer is whatever that system was
/// composed with: the reviewed egress client under a selection the surface assembled, and the
/// port's own `Unserved` under a system nobody attached one to. No client is constructed here, no
/// second path exists, and the fail-closed direction is still the default.
///
/// That confinement does not itself confine an HTTP request is the same call [`GuardedMetrics`]
/// makes below: the peer confines what this process *spawns*, and a request made in this process
/// against this machine's network is the same request the composed `System` would make. Refusing it
/// would cost a capability without adding a boundary.
impl GuardedHttp for SandboxedSystem {
    fn serves_http(&self) -> bool {
        GuardedHttp::serves_http(&self.inner)
    }

    fn http_request<'a>(
        &'a self,
        request: &'a crate::port::HttpRequest,
        allow: &'a PrivateNetAllow,
    ) -> Guarded<'a, crate::port::HttpResponse> {
        GuardedHttp::http_request(&self.inner, request, allow)
    }
}

/// Metrics delegate rather than deny (C-653).
///
/// The peer confines what this process *spawns*; a metric read happens in this process, against
/// this machine, through the composed system's own narrowable `/proc`+`/sys` roots. So it is the
/// same host and the same measurement — refusing here would be a false negative about a substrate
/// flux can genuinely measure, and the seam's contract is that an unavailable metric is explicitly
/// unavailable rather than absent for the wrong reason.
impl GuardedMetrics for SandboxedSystem {
    fn served_metric_kinds(&self) -> Vec<MetricKind> {
        GuardedMetrics::served_metric_kinds(&self.inner)
    }

    fn read_metric(&self, kind: MetricKind) -> Guarded<'_, MetricAnswer> {
        GuardedMetrics::read_metric(&self.inner, kind)
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

    /// The **nested-flux fixture**: a `System` whose *ambient* sandbox already concluded that an
    /// outer flux sandbox confines this tree. That is the state a child flux is really in — the
    /// parent exports `FLUX_SANDBOX=require` beside the marker (`sandbox::posture_env` sends
    /// nothing at all when the parent's mode is `Off`), and the child's own startup resolves
    /// `AlreadyConfined` and discloses it.
    ///
    /// Reproducing it through `Sandbox::resolve` rather than by hand keeps the fixture honest: the
    /// admission path under test is the same one production reaches, and it needs no wrapper
    /// binary, so nothing here is a property of the machine.
    fn nested_under_outer_flux(system: System) -> System {
        std::env::set_var("FLUX_SANDBOXED", "1");
        let ambient = Sandbox::resolve(SandboxSettings {
            mode: SandboxMode::Require,
            ..SandboxSettings::off()
        });
        assert!(
            ambient.confined_by_parent(),
            "the fixture must really be confined by a parent, or it proves nothing"
        );
        system.with_sandbox(ambient)
    }

    /// Compose a peer **without** admission, to test the identity projection separately from the
    /// gate that guards it.
    ///
    /// Deliberately test-only and deliberately unchecked: `substrate_identity` must report
    /// whatever posture it is handed, and the only way to prove it is not returning a constant is
    /// to hand it more than one — including postures admission would (correctly) refuse. Every
    /// *admitted* path is exercised through the real doors above and below this.
    fn unadmitted(system: System, sandbox: Sandbox) -> SandboxedSystem {
        SandboxedSystem {
            inner: system.with_sandbox(sandbox),
        }
    }

    /// C-651 review round 1, **the blocking finding**: a bare `FLUX_SANDBOXED` marker must not
    /// admit this peer where the posture that would disclose it is not in effect.
    ///
    /// `Sandbox::resolve` deliberately makes the marker inert at mode `Off` — "must not be re-read
    /// as 'confined by a parent' just because a stray `FLUX_SANDBOXED` is set". Re-resolving at
    /// `Require` to build the peer revived it, and in the attended default posture
    /// (`flux --host boxed`, nothing else) `apply_sandbox_env` returns early at `Off`, so the
    /// outer-confinement audit line that every *other* marker-trusting path pays never fires. The
    /// result was a peer that reported "confined by parent flux", wrapped nothing, and said
    /// nothing.
    ///
    /// So the marker is trusted through exactly one channel: an ambient posture that already
    /// reached that conclusion itself (and therefore already disclosed it). A peer-forced
    /// resolution admits only a backend it discovered and probed.
    #[test]
    fn a_bare_outer_confinement_marker_does_not_admit_the_peer() {
        let _env = EnvGuard::new(SANDBOX_KEYS);
        let (root, system) = fixture("sandboxed-forged-marker");
        // The attended default: nothing asked for confinement, so `System::new`'s sandbox is `Off`
        // and no startup disclosure was owed or made. Only the marker is set — stale, inherited
        // from an unrelated process, or forged.
        std::env::set_var("FLUX_SANDBOXED", "1");
        assert!(
            !system.sandbox().confined_by_parent(),
            "the ambient posture must not have trusted the marker, or this proves nothing"
        );

        let error = SandboxedSystem::from_env(system)
            .expect_err("a marker the ambient posture never trusted must not admit the peer");

        let text = error.to_string();
        assert!(
            text.contains("fails closed"),
            "the refusal must use the vocabulary the rest of the surface uses: {text}"
        );
        assert!(
            text.contains("FLUX_SANDBOXED"),
            "the refusal must name the marker it refused, so a stale one can be cleared: {text}"
        );

        std::fs::remove_dir_all(&root).ok();
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

    /// The one channel that may trust the marker: a nested flux, whose *ambient* posture already
    /// concluded it and whose startup already disclosed it. This is the admitted-peer path, and it
    /// is deterministic on every platform — so acceptance 4's identity claim rests on a peer that
    /// came through the real door, not through the test-only one below.
    #[test]
    fn from_env_inherits_an_ambient_outer_confinement_it_did_not_establish() {
        let _env = EnvGuard::new(SANDBOX_KEYS);
        let (root, system) = fixture("sandboxed-nested");
        let system = nested_under_outer_flux(system);

        let peer = SandboxedSystem::from_env(system)
            .expect("an ambient posture that already trusted the marker admits the peer");
        let identity = peer.substrate_identity();

        assert_eq!(identity.kind, "sandboxed");
        assert_eq!(
            identity.confinement, "sandbox: confined by parent flux",
            "the inherited posture must be reported as what it is"
        );
        assert!(!identity.remotely_reported);

        std::fs::remove_dir_all(&root).ok();
    }

    /// C-651 (acceptance 4) — the peer reports its confinement **truthfully**: the string is the
    /// composed sandbox's own `describe()`, not a claim this type invents.
    ///
    /// Proved by handing the same projection *different* postures and requiring different answers,
    /// each equal to the composed system's own — which is what a hardcoded string would fail. The
    /// review of round 1 was right that the earlier single-posture version could not have caught
    /// one. Only the kind may differ from the native identity, and only ever in one direction.
    #[test]
    fn the_peer_reports_the_composed_sandbox_confinement_not_a_claim_of_its_own() {
        let _env = EnvGuard::new(SANDBOX_KEYS);
        let (root, system) = fixture("sandboxed-identity");

        let mut seen = Vec::new();
        for (label, sandbox) in [
            ("disabled", Sandbox::disabled()),
            ("outer flux", {
                std::env::set_var("FLUX_SANDBOXED", "1");
                Sandbox::resolve(SandboxSettings {
                    mode: SandboxMode::Require,
                    ..SandboxSettings::off()
                })
            }),
            ("unsupported under require", {
                std::env::remove_var("FLUX_SANDBOXED");
                std::env::set_var("FLUX_BWRAP_BIN", "/nonexistent/bwrap");
                Sandbox::resolve(SandboxSettings {
                    mode: SandboxMode::Require,
                    ..SandboxSettings::off()
                })
            }),
        ] {
            let peer = unadmitted(system.clone(), sandbox);
            let identity = peer.substrate_identity();
            let native = ExecutionIdentity::substrate_identity(peer.system());

            assert_eq!(identity.kind, "sandboxed", "{label}");
            assert_eq!(native.kind, "native", "{label}: the inner system is native");
            assert!(!identity.remotely_reported, "{label}");
            assert_eq!(
                identity.confinement, native.confinement,
                "{label}: the peer must report the composed sandbox's posture verbatim"
            );
            assert_eq!(identity.workspace, native.workspace, "{label}");
            seen.push(identity.confinement);
        }

        // The point of the loop: three postures, three different strings. A constant — or a string
        // derived from anything but the composed sandbox — cannot satisfy this.
        seen.dedup();
        assert_eq!(
            seen.len(),
            3,
            "the confinement report must vary with the composed sandbox: {seen:?}"
        );
        assert!(
            seen[0].contains("off") && seen[1].contains("confined by parent"),
            "and each must be that posture's own words: {seen:?}"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// The reported kind is **load-bearing**, not decoration, so every admission path must report
    /// the same one.
    ///
    /// C-652's `Executor::non_native_target` reads `kind != "native"` as "a substrate selection is
    /// in force" and hides `browser.*` / `web.crawl` on that basis. A path that admitted a peer
    /// while reporting `native` would silently re-expose those operations underneath a *confined*
    /// selection — the exact inversion of what selecting confinement asked for. This walks every
    /// door into the type and requires one answer from all of them.
    #[test]
    fn every_admission_path_reports_the_same_non_native_kind() {
        let _env = EnvGuard::new(SANDBOX_KEYS);
        let (root, system) = fixture("sandboxed-kind");

        assert_ne!(
            KIND, "native",
            "the kind is what tells the executor a selection is in force"
        );

        // Door 1: the inherited-outer-confinement path, deterministic everywhere.
        let nested = SandboxedSystem::from_env(nested_under_outer_flux(system.clone()))
            .expect("the nested fixture admits");
        assert_eq!(nested.substrate_identity().kind, KIND);

        // Door 2 and 3: `from_env`'s own resolution, and the `resolve` primitive, both of which
        // need a real discovered backend. Exercised wherever this machine has one; the assertion
        // is the same, and the paths that *cannot* run here are covered by the refusal tests.
        std::env::remove_var("FLUX_SANDBOXED");
        let discovered = Sandbox::resolve(SandboxSettings {
            mode: SandboxMode::Require,
            ..SandboxSettings::off()
        });
        if discovered.is_active() {
            let resolved = SandboxedSystem::resolve(system.clone(), discovered)
                .expect("an active backend admits through the primitive");
            assert_eq!(resolved.substrate_identity().kind, KIND);

            let from_env = SandboxedSystem::from_env(system)
                .expect("an active backend admits through the production constructor");
            assert_eq!(from_env.substrate_identity().kind, KIND);
            assert!(
                from_env.substrate_identity().confinement.contains("active"),
                "a self-resolved backend must report its own wrapper, not an inherited posture"
            );
        }

        std::fs::remove_dir_all(&root).ok();
    }

    /// Review round 1, minor 2 — the **consequence of selecting a substrate**, pinned rather than
    /// only documented.
    ///
    /// An admitted peer is a *snapshot*: it owns its `System` by value, and `ExecutionEnvironment`
    /// hands that same snapshot back for the life of the session. So a workspace transition
    /// (`git_worktree_enter`, `fleet.isolate`) re-roots the native path while the selected
    /// substrate keeps reporting — and operating on — the root it was admitted with. Nothing
    /// refuses the transition; the two views simply diverge.
    ///
    /// That is what selecting a substrate has always meant (a `remote` binding behaves the same),
    /// and it is why the posture floor raises only a *named* binding and never manufactures a
    /// selection where the operator made none. It is pinned here so a future change to that
    /// behaviour has to come past a test that states it.
    #[tokio::test]
    async fn an_admitted_peer_is_a_snapshot_and_does_not_follow_a_re_root() {
        let _env = EnvGuard::new(SANDBOX_KEYS);
        let (root, system) = fixture("sandboxed-snapshot");
        let elsewhere = fixture_dir("sandboxed-snapshot-worktree");
        std::fs::write(elsewhere.join("only-here.txt"), "after the transition").unwrap();

        let peer = SandboxedSystem::from_env(nested_under_outer_flux(system))
            .expect("the nested fixture admits");
        let admitted_root = peer.substrate_identity().workspace;

        // The native path transitions; the selected substrate is not consulted and does not move.
        let moved = peer
            .system()
            .rerooted(&elsewhere)
            .expect("the native system re-roots");
        assert_eq!(
            moved.workspace().root(),
            elsewhere.canonicalize().unwrap(),
            "the re-rooted native system followed the transition"
        );
        assert_eq!(
            peer.substrate_identity().workspace,
            admitted_root,
            "the selected substrate stayed pinned to the root it was admitted with"
        );

        // And that is not cosmetic: the file that exists only after the transition is unreachable
        // through the peer, which is exactly the divergence an operator has to know about.
        let port: &dyn ExecutionSystem = &peer;
        assert!(
            port.read_file("only-here.txt").await.is_err(),
            "the pinned substrate must not silently reach the post-transition root"
        );
        assert_eq!(
            moved.read_file("only-here.txt").await.unwrap(),
            "after the transition"
        );

        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&elsewhere).ok();
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

        let peer = SandboxedSystem::from_env(nested_under_outer_flux(system))
            .expect("the nested fixture admits");
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

        // Metrics delegate — the peer measures the machine it actually runs on, and says so
        // through the same kinds the composed system serves.
        assert_eq!(
            GuardedMetrics::served_metric_kinds(&peer),
            GuardedMetrics::served_metric_kinds(peer.system()),
            "a metric read happens in this process, on this machine"
        );

        // HTTP does not: nothing attached a backend to the composed system here, so the delegation
        // lands on the port's own refusal (C-675 changed who may serve this family, not what an
        // unattached substrate answers). A sandboxed selection still cannot silently borrow the
        // caller's process to make the request. A loopback literal pins without a DNS lookup, so
        // this is about the port's answer rather than about a network.
        let target = crate::net::guard_url_scoped_for_secret(
            "http://127.0.0.1:9/probe",
            &PrivateNetAllow::Any,
        )
        .expect("a loopback literal is admitted under an `Any` grant");
        let request = crate::port::HttpRequest {
            operation: "http.request".into(),
            method: "GET".into(),
            target,
            headers: Vec::new(),
            body: None,
            timeout: Duration::from_secs(1),
            max_response_bytes: 1024,
            secrets: crate::port::HttpSecretScope::default(),
        };
        let http = port
            .http_request(&request, &PrivateNetAllow::Any)
            .await
            .expect_err("a peer composed over an unattached system must not serve HTTP");
        assert!(
            http.to_string().starts_with(crate::port::UNSERVED),
            "the refusal must be the port's own `Unserved`, not an improvised error: {http}"
        );

        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    /// C-675 (acceptance 1 and 4) — the peer serves HTTP by **delegating to the system it
    /// composes**, exactly as it serves every other family.
    ///
    /// The composed `System` is what a composition site attaches a backend to, so this is the whole
    /// mechanism: the surface that holds the workspace's one egress client hands it down, and the
    /// peer forwards to it. Both directions are asserted, because the interesting property is not
    /// "it can serve" but "it serves *that*, and nothing when there is nothing" — a peer that
    /// improvised a client would pass the first half and fail the second, and one that kept the
    /// old empty impl would fail the first.
    #[tokio::test]
    async fn the_peer_serves_the_http_backend_attached_to_the_composed_system() {
        struct Attached(std::sync::Mutex<Vec<String>>);

        impl GuardedHttp for Attached {
            fn http_request<'a>(
                &'a self,
                request: &'a crate::port::HttpRequest,
                _allow: &'a PrivateNetAllow,
            ) -> Guarded<'a, crate::port::HttpResponse> {
                self.0.lock().unwrap().push(request.operation.clone());
                Box::pin(async {
                    Ok(crate::port::HttpResponse {
                        status: 204,
                        headers: Vec::new(),
                        body: Vec::new(),
                        truncated: false,
                        admits: Vec::new(),
                    })
                })
            }
        }

        let _env = EnvGuard::new(SANDBOX_KEYS);
        let (root, system) = fixture("sandboxed-http");
        let backend = std::sync::Arc::new(Attached(std::sync::Mutex::new(Vec::new())));

        let peer =
            SandboxedSystem::from_env(nested_under_outer_flux(system).with_http(backend.clone()))
                .expect("the nested fixture admits");
        let port: &dyn ExecutionSystem = &peer;

        let target = crate::net::guard_url_scoped_for_secret(
            "http://127.0.0.1:9/probe",
            &PrivateNetAllow::Any,
        )
        .expect("a loopback literal is admitted under an `Any` grant");
        let request = crate::port::HttpRequest {
            operation: "web.fetch".into(),
            method: "GET".into(),
            target,
            headers: Vec::new(),
            body: None,
            timeout: Duration::from_secs(1),
            max_response_bytes: 1024,
            secrets: crate::port::HttpSecretScope::default(),
        };

        let served = port
            .http_request(&request, &PrivateNetAllow::Any)
            .await
            .expect("the peer serves the backend its composed system carries");
        assert_eq!(served.status, 204);
        assert_eq!(
            backend.0.lock().unwrap().as_slice(),
            ["web.fetch".to_string()],
            "the peer must forward to the attached backend rather than improvise a client"
        );

        std::fs::remove_dir_all(&root).ok();
    }
}
