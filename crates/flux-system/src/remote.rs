//! Serving the guarded-IO [`port`](crate::port) by **delegating to another substrate** (C-399).
//!
//! [`port`](crate::port) names *"a remote executor"* among the substrates it exists for. This module
//! is that substrate: [`RemoteSystem`] implements all four port families by handing each operation to
//! a [`Delegate`] and turning what comes back into a `flux_core::Result`. A caller therefore runs
//! operations somewhere other than its own process while the guarantees stay stated in exactly one
//! place — `port.rs` — because nothing here re-states them.
//!
//! **This is not a second IO path**, for the same reason the port is not: `RemoteSystem` cannot open
//! a file or start a process. It can only ask something else to, and that something else is
//! whatever the far side already was. The port made the caller substitutable; this makes the
//! *callee* substitutable, and neither touches the guard.
//!
//! ## No wire format is chosen here, deliberately
//!
//! [`Delegate`] **is** the delegation seam, and it is a Rust trait rather than a protocol. There is
//! no serialization, no framing, no transport and no dependency added to this crate: a caller that
//! has an HTTP client, a plugin frame, a Unix socket or a `flux-exchange` channel implements
//! `Delegate` over it and gets the port. That keeps this story to what its Acceptance is about — the
//! failure semantics of delegation — and leaves the open question in
//! `docs/designs/remote-agents.md` (is the remote wire a channel API or a port delegation?) open,
//! which is where it currently belongs. A wire format invented here would have pre-answered it.
//!
//! ## Local-first: no service is required
//!
//! [`Loopback`] serves `Delegate` from any in-process substrate, so [`RemoteSystem::loopback`] gives
//! the whole delegation path with **nothing running** — a developer exercises it against a native
//! [`System`](crate::System) on their own machine. That is `docs/vision.md`'s local-first principle
//! applied to the runtime axis: a capability that only exists when a platform is reachable is a
//! capability the personal coding agent does not have. A `Loopback` also never reports an
//! unreachable link, because there is no link to break.
//!
//! ## The three failure modes, and why they are kept apart
//!
//! An operator responds to these in *opposite* ways, so collapsing them would make the backend
//! actively misleading — worse than one that reported nothing:
//!
//! | Mode | What happened | What an operator does |
//! |---|---|---|
//! | [`FailureMode::Refused`] | The far side answered, and the answer was no. A guard did its job. | Fix the request, or widen the grant. **Do not** retry unchanged. |
//! | [`FailureMode::Unreachable`] | No answer arrived. Whether the operation happened is **unknown**. | Investigate the link. Retrying is meaningful. |
//! | [`FailureMode::Unserved`] | The delegate does not implement this operation at all. | Implement it, or stop asking. Retrying never helps. |
//!
//! [`failure_mode`] recovers the mode from `flux_core::Error`'s typed guarded-IO variant, so a
//! consumer holding nothing but the port's `Result` can still branch on it.
//!
//! ### The classification is structural, not textual
//!
//! A delegate reports a refusal by *returning* [`Answer::Refused`] and a broken link by returning
//! [`Unreachable`] — two different positions in the type, not two different strings. Only the
//! transport can produce `Unreachable`, and [`settle`] stores that distinction in
//! `flux_core::Error::GuardedIo`. The diagnostic prefix is presentation only. So a delegate whose
//! refusal reason begins with the exact unreachable diagnostic still classifies as a refusal. This
//! matters: an operator who saw "unreachable" for a guard refusal would go and investigate a
//! perfectly healthy network.
//!
//! ### An answered failure defaults to a refusal
//!
//! An error the far side returned that is *not* recognizably unserved classifies as
//! [`FailureMode::Refused`] — including a plain "no such file". From the operator's seat that is
//! accurate (the link worked; the operation did not) and it is the safe direction: an unrecognized
//! failure never gets to claim the link is broken, so "unreachable" keeps meaning what it says.
//!
//! ## Fail-closed, from "serves nothing" upward
//!
//! [`Delegate`] has **no required methods**. Bringing a substrate up therefore starts from a
//! delegate that serves nothing and denies everything, and each capability is added deliberately —
//! the same posture `port.rs` takes and the same one the plugin host takes with manifest grants. The
//! defaults deny in the port's own words (`this guarded substrate cannot …`), including for the
//! operations a well-meaning implementation would answer with a *value*: `path_exists` and `is_dir`
//! must not guess `false`, and `list_dir` / `walk_files` must not return an empty listing, because
//! those are wrong answers rather than missing features and callers act on them.
//!
//! The reduction graph is mirrored from `port.rs` rather than re-derived: a delegate that serves only
//! `run_with_env` still answers `run`, and one that serves only `write_file_bytes` still answers
//! `write_file`, byte-identically. Where `port.rs` refuses to reduce (an append is not a
//! read-then-write; a capped read is not a full read plus a truncate), so does this.
//!
//! ## What this module does not do
//!
//! - **No network port.** There is no guarded-network trait to delegate yet — that is C-435 — so
//!   egress is absent from `Delegate` rather than approximated in it.
//! - **No long-lived children.** `spawn_background` hands back a [`ManagedChild`](crate::ManagedChild)
//!   owning a real `tokio::process::Child`, which no wire can carry. `RemoteSystem` leaves
//!   `port.rs`'s denial in place rather than pretending to have spawned something.
//! - **No relaxation.** `RemoteSystem` adds no permission of its own. Where the far side is a native
//!   `System`, the workspace jail, argv-only spawning and env clearing are exactly the far side's,
//!   and an escape it refuses is refused through the delegation too.
//!
//! ## The cost this pays on purpose
//!
//! Being in-repo, these four impls cost reviewed entries in `flux-codegate`'s
//! `no_unreviewed_guarded_port_backend_outside_system` allow-list. C-399 accepted that deliberately:
//! the alternative was an unreviewed backend living somewhere the gate cannot see.

use std::sync::Arc;
use std::time::Duration;

pub use flux_core::{Error, GuardedIoError, GuardedIoFailure as FailureMode, Result};

use crate::port::{Guarded, GuardedEnv, GuardedHostFiles, GuardedProcess, GuardedWorkspaceFiles};
use crate::{OutputObserver, ProcessOutput, ScopedFileRead};

/// The failure mode behind a guarded error, or `None` if it did not come from a delegated operation.
///
/// Matching is on the shared error variant, never on formatted text, so a refusal whose reason
/// begins with another mode's canonical prefix is still a refusal. `None` means the error is not one
/// this module produced (an unrelated `Error::Io`, say), which a consumer should treat as it would
/// any other error rather than as a fourth mode.
pub fn failure_mode(error: &Error) -> Option<FailureMode> {
    match error {
        Error::GuardedIo(failure) => Some(failure.kind()),
        _ => None,
    }
}

/// A delegated operation reached the far side and came back with one of these.
///
/// The variants are positions in a type rather than strings, which is what makes the failure modes
/// forge-proof: a delegate chooses *which* answer it returns, and this module chooses how each one
/// reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer<T> {
    /// The operation ran and this is its result.
    Served(T),
    /// A guard on the far side said no. Carries the far side's reason, which is reported to the
    /// operator but never used to classify.
    Refused(String),
    /// The far side does not implement this operation. Carries the phrase completing
    /// "this guarded substrate cannot …", so an unserved operation names itself.
    Unserved(String),
}

/// **No answer arrived.** The link, not the operation, is what failed — so nothing is known about
/// whether the operation happened, and this is the one mode a delegate cannot produce by answering.
///
/// Constructing one is a transport's job: it is what a dial failure, a closed socket, a framing
/// error or a request timeout becomes.
#[derive(Debug, Clone)]
pub struct Unreachable(String);

impl Unreachable {
    /// Report that the delegate could not be reached, `detail` naming what went wrong on the wire.
    pub fn new(detail: impl Into<String>) -> Self {
        Self(detail.into())
    }
}

impl std::fmt::Display for Unreachable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unreachable {}

/// A transport error is an unreachable delegate, so `?` works in a `Delegate` implementation.
impl From<std::io::Error> for Unreachable {
    fn from(error: std::io::Error) -> Self {
        Self::new(error.to_string())
    }
}

/// What a delegated operation delivers: an [`Answer`] from the far side, or [`Unreachable`] if none
/// arrived. The two are separate `Result` arms precisely so no code path can confuse them.
pub type Delivered<T> = std::result::Result<Answer<T>, Unreachable>;

/// The future an asynchronous delegated operation returns. Boxed for the same reason
/// [`Guarded`] is: the delegate has to be usable as `dyn`.
pub type Answered<'a, T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Delivered<T>> + Send + 'a>>;

/// The guarded surface a substrate has to serve to be delegable — the port's four families, bundled.
///
/// This follows `flux_plugin::PluginSystem`'s precedent rather than introducing a god trait: the
/// bundle is declared *at the consumer that spans the families*, and the operations themselves stay
/// in [`port`](crate::port). The blanket impl means the native [`System`](crate::System) satisfies
/// it for free, and so does a [`RemoteSystem`] — which is what makes a delegation chain typecheck.
pub trait GuardedSubstrate:
    GuardedProcess + GuardedHostFiles + GuardedEnv + GuardedWorkspaceFiles
{
}

impl<T> GuardedSubstrate for T where
    T: GuardedProcess + GuardedHostFiles + GuardedEnv + GuardedWorkspaceFiles + ?Sized
{
}

/// The far side of a [`RemoteSystem`] — one method per delegable port operation, every one optional.
///
/// Implement this over whatever carries your requests. Each method returns [`Delivered`], which
/// forces the implementor to say *which* kind of failure occurred instead of flattening both into
/// one error: an answered refusal is `Ok(Answer::Refused(..))`, a broken link is
/// `Err(Unreachable::new(..))`.
///
/// **Every default denies.** A delegate that implements nothing serves nothing, and the operations
/// that reduce to another one ([`run`](Self::run), [`read_file`](Self::read_file),
/// [`write_file`](Self::write_file), [`run_with_env_observed`](Self::run_with_env_observed)) reduce
/// exactly as `port.rs` reduces them, so serving one primitive earns the same derived operations the
/// native backend has.
pub trait Delegate: Send + Sync {
    // -- process ----------------------------------------------------------------------------------

    /// Execute `argv` (no shell) with `env` applied on top of the far side's minimal allow-list.
    fn run_with_env<'a>(
        &'a self,
        argv: &'a [String],
        env: &'a [(String, String)],
        timeout: Duration,
    ) -> Answered<'a, ProcessOutput> {
        let _ = (argv, env, timeout);
        unserved("run a process")
    }

    /// Execute `argv` with only the far side's minimal environment. Reduces to
    /// [`run_with_env`](Self::run_with_env) with an empty `env`, as `port.rs` does.
    fn run<'a>(&'a self, argv: &'a [String], timeout: Duration) -> Answered<'a, ProcessOutput> {
        self.run_with_env(argv, &[], timeout)
    }

    /// [`run_with_env`](Self::run_with_env) with a live line observer.
    ///
    /// The observer is a view onto captured output, never a second channel, so the default — run
    /// without it — returns a byte-identical result and loses only progress reporting. A transport
    /// that can stream lines back overrides this.
    fn run_with_env_observed<'a>(
        &'a self,
        argv: &'a [String],
        env: &'a [(String, String)],
        timeout: Duration,
        observer: OutputObserver,
    ) -> Answered<'a, ProcessOutput> {
        let _ = observer;
        self.run_with_env(argv, env, timeout)
    }

    /// Execute `argv`, feeding `stdin` to the child and closing it.
    ///
    /// Not reducible: silently dropping the payload would let a `git apply -` report success over an
    /// empty patch, so a transport that cannot carry a child's stdin must leave this denying.
    fn run_with_stdin<'a>(
        &'a self,
        argv: &'a [String],
        stdin: &'a [u8],
        timeout: Duration,
    ) -> Answered<'a, ProcessOutput> {
        let _ = (argv, stdin, timeout);
        unserved("feed a child process stdin")
    }

    // -- host files -------------------------------------------------------------------------------

    /// Reduce a host path to its physical identity on the far side. Synchronous, mirroring
    /// [`GuardedHostFiles::host_path_identity`].
    fn host_path_identity(&self, path: &str) -> Delivered<String> {
        let _ = path;
        Ok(unserved_now("reduce a host path to its identity"))
    }

    /// Read a host file through an explicit `scope`, capped at `max_bytes`.
    fn read_file_scoped<'a>(
        &'a self,
        path: &'a str,
        scope: &'a str,
        max_bytes: usize,
    ) -> Answered<'a, ScopedFileRead> {
        let _ = (path, scope, max_bytes);
        unserved("read a scoped host file")
    }

    // -- env --------------------------------------------------------------------------------------

    /// The value of `key` in the far side's guarded environment.
    fn env(&self, key: &str) -> Delivered<Option<String>> {
        let _ = key;
        Ok(unserved_now("read the guarded environment"))
    }

    // -- workspace files --------------------------------------------------------------------------

    /// The raw bytes of a workspace file on the far side.
    fn read_file_bytes<'a>(&'a self, path: &'a str) -> Answered<'a, Vec<u8>> {
        let _ = path;
        unserved("read a workspace file")
    }

    /// Write raw bytes to a workspace file on the far side.
    fn write_file_bytes<'a>(&'a self, path: &'a str, contents: &'a [u8]) -> Answered<'a, ()> {
        let _ = (path, contents);
        unserved("write a workspace file")
    }

    /// A workspace file decoded as UTF-8. Reduces to
    /// [`read_file_bytes`](Self::read_file_bytes) with `port.rs`'s decode and `port.rs`'s error.
    fn read_file<'a>(&'a self, path: &'a str) -> Answered<'a, String> {
        Box::pin(async move {
            match self.read_file_bytes(path).await? {
                Answer::Served(bytes) => match String::from_utf8(bytes) {
                    Ok(text) => Ok(Answer::Served(text)),
                    Err(_) => Ok(Answer::Refused(format!("{path}: not valid UTF-8"))),
                },
                Answer::Refused(detail) => Ok(Answer::Refused(detail)),
                Answer::Unserved(what) => Ok(Answer::Unserved(what)),
            }
        })
    }

    /// Write UTF-8 text to a workspace file. Reduces to
    /// [`write_file_bytes`](Self::write_file_bytes) byte-identically.
    fn write_file<'a>(&'a self, path: &'a str, contents: &'a str) -> Answered<'a, ()> {
        self.write_file_bytes(path, contents.as_bytes())
    }

    /// Append text to a workspace file. Not reducible: read-then-write is not an append.
    fn append_file<'a>(&'a self, path: &'a str, contents: &'a str) -> Answered<'a, ()> {
        let _ = (path, contents);
        unserved("append to a workspace file")
    }

    /// At most `max` bytes of a **regular** workspace file, as `(bytes, truncated)`. Not reducible:
    /// truncating a full read loses both the memory bound and the refusal of a FIFO or device.
    fn read_file_bytes_capped<'a>(
        &'a self,
        path: &'a str,
        max: usize,
    ) -> Answered<'a, (Vec<u8>, bool)> {
        let _ = (path, max);
        unserved("read a workspace file under a byte cap")
    }

    /// The byte size of a workspace file, as a metadata call — never as a read.
    fn file_size<'a>(&'a self, path: &'a str) -> Answered<'a, u64> {
        let _ = path;
        unserved("stat a workspace file's size")
    }

    /// Whether a path exists on the far side. Denies rather than guessing `false`: a caller asks
    /// this to decide whether it is creating or overwriting.
    fn path_exists<'a>(&'a self, path: &'a str) -> Answered<'a, bool> {
        let _ = path;
        unserved("test whether a workspace path exists")
    }

    /// Whether a workspace path is a directory. Denies rather than guessing `false`, which would
    /// send the caller down the file path.
    fn is_dir<'a>(&'a self, path: &'a str) -> Answered<'a, bool> {
        let _ = path;
        unserved("test whether a workspace path is a directory")
    }

    /// The last-modification time of a workspace file. Denies rather than fabricating a timestamp,
    /// which would defeat a read-before-write guard rather than degrade it.
    fn file_mtime<'a>(&'a self, path: &'a str) -> Answered<'a, std::time::SystemTime> {
        let _ = path;
        unserved("read a workspace file's modification time")
    }

    /// The entry names of a workspace directory, sorted. Denies rather than returning an empty
    /// listing, which is a wrong answer ("the directory is empty") that callers act on.
    fn list_dir<'a>(&'a self, path: &'a str) -> Answered<'a, Vec<String>> {
        let _ = path;
        unserved("list a workspace directory")
    }

    /// Files under a workspace directory, recursively, capped at `max`. Denies for the same reason
    /// [`list_dir`](Self::list_dir) does.
    fn walk_files<'a>(&'a self, base: &'a str, max: usize) -> Answered<'a, Vec<String>> {
        let _ = (base, max);
        unserved("walk a workspace directory")
    }
}

/// An unserved answer for an asynchronous operation, phrased to complete `port.rs`'s
/// "this guarded substrate cannot …".
fn unserved<'a, T: Send + 'a>(operation: &'static str) -> Answered<'a, T> {
    Box::pin(async move { Ok(Answer::Unserved(operation.to_string())) })
}

/// [`unserved`] for the synchronous operations, which have no future to put it in.
fn unserved_now<T>(operation: &'static str) -> Answer<T> {
    Answer::Unserved(operation.to_string())
}

/// Turn what a delegate delivered into the port's `Result`, preserving the failure mode in the
/// shared error variant so [`failure_mode`] never has to interpret operator-facing text.
fn settle<T>(delivered: Delivered<T>) -> Result<T> {
    match delivered {
        Ok(Answer::Served(value)) => Ok(value),
        Ok(Answer::Refused(detail)) => Err(Error::GuardedIo(GuardedIoError::new(
            FailureMode::Refused,
            detail,
        ))),
        Ok(Answer::Unserved(what)) => Err(Error::GuardedIo(GuardedIoError::new(
            FailureMode::Unserved,
            what,
        ))),
        Err(unreachable) => Err(Error::GuardedIo(GuardedIoError::new(
            FailureMode::Unreachable,
            unreachable.0,
        ))),
    }
}

/// The guarded-IO port, served by a [`Delegate`].
///
/// Every operation is a straight hand-off: this type holds no workspace, opens no file and starts no
/// process, so it can neither add a permission nor remove one. What it does own is the failure
/// semantics — see the module docs.
pub struct RemoteSystem {
    delegate: Arc<dyn Delegate>,
}

impl RemoteSystem {
    /// Serve the port from `delegate`.
    pub fn new(delegate: Arc<dyn Delegate>) -> Self {
        Self { delegate }
    }

    /// Serve the port by delegating to an **in-process** substrate — the local-first path, which
    /// needs no service running and cannot report an unreachable link.
    pub fn loopback<T: GuardedSubstrate + ?Sized + 'static>(substrate: Arc<T>) -> Self {
        Self::new(Arc::new(Loopback::new(substrate)))
    }

    /// The delegate this backend serves from, for a caller that needs to swap or inspect it.
    pub fn delegate(&self) -> &Arc<dyn Delegate> {
        &self.delegate
    }

    /// [`GuardedEnv::env`] with the failure mode preserved.
    ///
    /// `env` returns `Option<String>`, which has nowhere to put the distinction — so it fails the
    /// credential closed as `None` for **both** a refusal and a broken link, exactly as `port.rs`
    /// specifies for a substrate with no environment. That is right for the caller (a missing
    /// credential must not resolve) and useless for the operator, who cannot tell a denied env read
    /// from a dead link. This is the escape hatch that keeps them apart; it is inherent rather than
    /// on the trait because widening [`GuardedEnv`] is not this story's to do.
    pub fn env_checked(&self, key: &str) -> Result<Option<String>> {
        settle(self.delegate.env(key))
    }
}

impl std::fmt::Debug for RemoteSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteSystem").finish_non_exhaustive()
    }
}

impl GuardedEnv for RemoteSystem {
    /// Fails the credential closed on either failure mode; [`RemoteSystem::env_checked`] is where
    /// the distinction survives.
    fn env(&self, key: &str) -> Option<String> {
        self.env_checked(key).ok().flatten()
    }
}

// `spawn_background` is deliberately left at `port.rs`'s denial: a `ManagedChild` owns a real
// `tokio::process::Child`, which no wire can carry, so there is nothing to delegate.
impl GuardedProcess for RemoteSystem {
    fn run_with_env<'a>(
        &'a self,
        argv: &'a [String],
        env: &'a [(String, String)],
        timeout: Duration,
    ) -> Guarded<'a, ProcessOutput> {
        Box::pin(async move { settle(self.delegate.run_with_env(argv, env, timeout).await) })
    }

    fn run<'a>(&'a self, argv: &'a [String], timeout: Duration) -> Guarded<'a, ProcessOutput> {
        Box::pin(async move { settle(self.delegate.run(argv, timeout).await) })
    }

    fn run_with_env_observed<'a>(
        &'a self,
        argv: &'a [String],
        env: &'a [(String, String)],
        timeout: Duration,
        observer: OutputObserver,
    ) -> Guarded<'a, ProcessOutput> {
        Box::pin(async move {
            settle(
                self.delegate
                    .run_with_env_observed(argv, env, timeout, observer)
                    .await,
            )
        })
    }

    fn run_with_stdin<'a>(
        &'a self,
        argv: &'a [String],
        stdin: &'a [u8],
        timeout: Duration,
    ) -> Guarded<'a, ProcessOutput> {
        Box::pin(async move { settle(self.delegate.run_with_stdin(argv, stdin, timeout).await) })
    }
}

impl GuardedHostFiles for RemoteSystem {
    fn host_path_identity(&self, path: &str) -> Result<String> {
        settle(self.delegate.host_path_identity(path))
    }

    fn read_file_scoped<'a>(
        &'a self,
        path: &'a str,
        scope: &'a str,
        max_bytes: usize,
    ) -> Guarded<'a, ScopedFileRead> {
        Box::pin(
            async move { settle(self.delegate.read_file_scoped(path, scope, max_bytes).await) },
        )
    }
}

// Every operation, including the ones the trait would otherwise default, so the delegate is asked
// each time rather than denied on this side — a delegate that serves an optional operation is the
// whole reason to have one, and a missed override would silently deny it.
impl GuardedWorkspaceFiles for RemoteSystem {
    fn read_file_bytes<'a>(&'a self, path: &'a str) -> Guarded<'a, Vec<u8>> {
        Box::pin(async move { settle(self.delegate.read_file_bytes(path).await) })
    }

    fn write_file_bytes<'a>(&'a self, path: &'a str, contents: &'a [u8]) -> Guarded<'a, ()> {
        Box::pin(async move { settle(self.delegate.write_file_bytes(path, contents).await) })
    }

    fn read_file<'a>(&'a self, path: &'a str) -> Guarded<'a, String> {
        Box::pin(async move { settle(self.delegate.read_file(path).await) })
    }

    fn write_file<'a>(&'a self, path: &'a str, contents: &'a str) -> Guarded<'a, ()> {
        Box::pin(async move { settle(self.delegate.write_file(path, contents).await) })
    }

    fn append_file<'a>(&'a self, path: &'a str, contents: &'a str) -> Guarded<'a, ()> {
        Box::pin(async move { settle(self.delegate.append_file(path, contents).await) })
    }

    fn read_file_bytes_capped<'a>(
        &'a self,
        path: &'a str,
        max: usize,
    ) -> Guarded<'a, (Vec<u8>, bool)> {
        Box::pin(async move { settle(self.delegate.read_file_bytes_capped(path, max).await) })
    }

    fn file_size<'a>(&'a self, path: &'a str) -> Guarded<'a, u64> {
        Box::pin(async move { settle(self.delegate.file_size(path).await) })
    }

    fn path_exists<'a>(&'a self, path: &'a str) -> Guarded<'a, bool> {
        Box::pin(async move { settle(self.delegate.path_exists(path).await) })
    }

    fn is_dir<'a>(&'a self, path: &'a str) -> Guarded<'a, bool> {
        Box::pin(async move { settle(self.delegate.is_dir(path).await) })
    }

    fn file_mtime<'a>(&'a self, path: &'a str) -> Guarded<'a, std::time::SystemTime> {
        Box::pin(async move { settle(self.delegate.file_mtime(path).await) })
    }

    fn list_dir<'a>(&'a self, path: &'a str) -> Guarded<'a, Vec<String>> {
        Box::pin(async move { settle(self.delegate.list_dir(path).await) })
    }

    fn walk_files<'a>(&'a self, base: &'a str, max: usize) -> Guarded<'a, Vec<String>> {
        Box::pin(async move { settle(self.delegate.walk_files(base, max).await) })
    }
}

/// A [`Delegate`] over an **in-process** substrate: the local-first far side, and the one used to
/// exercise the delegation path with no service running.
///
/// Because there is no wire, this delegate never produces [`Unreachable`] — every operation reaches
/// the substrate. What it does do is **re-classify**: an error the substrate returned is mapped back
/// into an [`Answer`], with [`failure_mode`] deciding which. That is what keeps a mode intact across
/// a hop, so "nobody implements this" does not become "the guard said no" one delegation later and
/// send an operator into an unbounded retry.
pub struct Loopback<T: ?Sized> {
    substrate: Arc<T>,
}

impl<T: GuardedSubstrate + ?Sized> Loopback<T> {
    /// Delegate to `substrate`, in this process.
    pub fn new(substrate: Arc<T>) -> Self {
        Self { substrate }
    }
}

/// Map a substrate's `Result` back into an [`Answer`], preserving the failure mode.
///
/// An unrecognized error becomes [`Answer::Refused`]: the substrate answered, so the link is not in
/// question. An error already marked unreachable stays unreachable — a `Loopback` does not
/// manufacture that mode, but it must not swallow one that arrived from a real wire further in.
fn relay<T>(result: Result<T>) -> Delivered<T> {
    match result {
        Ok(value) => Ok(Answer::Served(value)),
        Err(Error::GuardedIo(failure)) => match failure.kind() {
            FailureMode::Unreachable => Err(Unreachable::new(failure.detail())),
            FailureMode::Unserved => Ok(Answer::Unserved(failure.detail().to_string())),
            FailureMode::Refused => Ok(Answer::Refused(failure.detail().to_string())),
        },
        // An ordinary failure came back from the far side, so it is an answered refusal rather than
        // evidence that the transport broke.
        Err(error) => Ok(Answer::Refused(error.to_string())),
    }
}

impl<T: GuardedSubstrate + ?Sized> Delegate for Loopback<T> {
    fn run_with_env<'a>(
        &'a self,
        argv: &'a [String],
        env: &'a [(String, String)],
        timeout: Duration,
    ) -> Answered<'a, ProcessOutput> {
        Box::pin(async move { relay(self.substrate.run_with_env(argv, env, timeout).await) })
    }

    fn run<'a>(&'a self, argv: &'a [String], timeout: Duration) -> Answered<'a, ProcessOutput> {
        Box::pin(async move { relay(self.substrate.run(argv, timeout).await) })
    }

    fn run_with_env_observed<'a>(
        &'a self,
        argv: &'a [String],
        env: &'a [(String, String)],
        timeout: Duration,
        observer: OutputObserver,
    ) -> Answered<'a, ProcessOutput> {
        Box::pin(async move {
            relay(
                self.substrate
                    .run_with_env_observed(argv, env, timeout, observer)
                    .await,
            )
        })
    }

    fn run_with_stdin<'a>(
        &'a self,
        argv: &'a [String],
        stdin: &'a [u8],
        timeout: Duration,
    ) -> Answered<'a, ProcessOutput> {
        Box::pin(async move { relay(self.substrate.run_with_stdin(argv, stdin, timeout).await) })
    }

    fn host_path_identity(&self, path: &str) -> Delivered<String> {
        relay(self.substrate.host_path_identity(path))
    }

    fn read_file_scoped<'a>(
        &'a self,
        path: &'a str,
        scope: &'a str,
        max_bytes: usize,
    ) -> Answered<'a, ScopedFileRead> {
        Box::pin(async move {
            relay(
                self.substrate
                    .read_file_scoped(path, scope, max_bytes)
                    .await,
            )
        })
    }

    fn env(&self, key: &str) -> Delivered<Option<String>> {
        Ok(Answer::Served(self.substrate.env(key)))
    }

    fn read_file_bytes<'a>(&'a self, path: &'a str) -> Answered<'a, Vec<u8>> {
        Box::pin(async move { relay(self.substrate.read_file_bytes(path).await) })
    }

    fn write_file_bytes<'a>(&'a self, path: &'a str, contents: &'a [u8]) -> Answered<'a, ()> {
        Box::pin(async move { relay(self.substrate.write_file_bytes(path, contents).await) })
    }

    fn read_file<'a>(&'a self, path: &'a str) -> Answered<'a, String> {
        Box::pin(async move { relay(self.substrate.read_file(path).await) })
    }

    fn write_file<'a>(&'a self, path: &'a str, contents: &'a str) -> Answered<'a, ()> {
        Box::pin(async move { relay(self.substrate.write_file(path, contents).await) })
    }

    fn append_file<'a>(&'a self, path: &'a str, contents: &'a str) -> Answered<'a, ()> {
        Box::pin(async move { relay(self.substrate.append_file(path, contents).await) })
    }

    fn read_file_bytes_capped<'a>(
        &'a self,
        path: &'a str,
        max: usize,
    ) -> Answered<'a, (Vec<u8>, bool)> {
        Box::pin(async move { relay(self.substrate.read_file_bytes_capped(path, max).await) })
    }

    fn file_size<'a>(&'a self, path: &'a str) -> Answered<'a, u64> {
        Box::pin(async move { relay(self.substrate.file_size(path).await) })
    }

    fn path_exists<'a>(&'a self, path: &'a str) -> Answered<'a, bool> {
        Box::pin(async move { relay(self.substrate.path_exists(path).await) })
    }

    fn is_dir<'a>(&'a self, path: &'a str) -> Answered<'a, bool> {
        Box::pin(async move { relay(self.substrate.is_dir(path).await) })
    }

    fn file_mtime<'a>(&'a self, path: &'a str) -> Answered<'a, std::time::SystemTime> {
        Box::pin(async move { relay(self.substrate.file_mtime(path).await) })
    }

    fn list_dir<'a>(&'a self, path: &'a str) -> Answered<'a, Vec<String>> {
        Box::pin(async move { relay(self.substrate.list_dir(path).await) })
    }

    fn walk_files<'a>(&'a self, base: &'a str, max: usize) -> Answered<'a, Vec<String>> {
        Box::pin(async move { relay(self.substrate.walk_files(base, max).await) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Prefixes remain distinct for readable diagnostics even though classification no longer reads
    /// them.
    #[test]
    fn the_three_markers_are_mutually_non_prefixing() {
        for (a, b) in [
            (
                FailureMode::Refused.prefix(),
                FailureMode::Unreachable.prefix(),
            ),
            (
                FailureMode::Refused.prefix(),
                FailureMode::Unserved.prefix(),
            ),
            (
                FailureMode::Unreachable.prefix(),
                FailureMode::Unserved.prefix(),
            ),
        ] {
            assert!(!a.starts_with(b) && !b.starts_with(a), "{a:?} vs {b:?}");
        }
    }

    /// Delegate-authored text is always detail, even when it quotes a canonical prefix. The typed
    /// kind remains the answer variant the delegate actually returned.
    #[test]
    fn a_marker_in_delegate_text_does_not_choose_the_kind() {
        let once = settle::<()>(Ok(Answer::Refused("denied".into())))
            .expect_err("a refusal is an error")
            .to_string();
        let twice = settle::<()>(Ok(Answer::Refused(once.clone())))
            .expect_err("a refusal is an error")
            .to_string();

        assert_ne!(
            once, twice,
            "a delegate-authored string is always quoted as detail"
        );
        assert_eq!(
            failure_mode(&settle::<()>(Ok(Answer::Refused(once))).unwrap_err()),
            Some(FailureMode::Refused),
            "a nested diagnostic cannot change the structural kind"
        );
    }

    /// `Unreachable` renders through its own marker, so a transport error that reaches an operator
    /// as a string is still classifiable.
    #[test]
    fn an_unreachable_delegate_renders_with_its_own_marker() {
        let error = settle::<()>(Err(Unreachable::new("timed out after 5s")))
            .expect_err("an unreachable delegate is an error");

        assert_eq!(failure_mode(&error), Some(FailureMode::Unreachable));
        assert!(error.to_string().ends_with("timed out after 5s"));
    }

    /// An error this module did not produce is not silently assigned a mode.
    #[test]
    fn an_unrelated_error_has_no_failure_mode() {
        assert_eq!(failure_mode(&Error::Other("something else".into())), None);
    }
}
