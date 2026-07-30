//! The guarded-IO **port**: what it means to *be* a `System`, as traits.
//!
//! [`System`] is the native implementor — an OS process, real syscalls, an OS sandbox. This module
//! states the same guarded operations as capability ports so a *non-native* substrate can serve them
//! instead: a WebAssembly embedder that answers through host imports, a remote executor, or a test
//! double. Nothing here relaxes a guarantee; each trait method's contract is the contract of the
//! inherent `System` method it mirrors, and the native impls at the bottom of this file are pure
//! delegation.
//!
//! **This is not a second IO path.** A port implementation gains no new ability to open a file or
//! start a process — those remain whatever the implementor could already do, and for flux's own
//! execution that is still exactly one place: `System::build_command`. The port makes the *caller*
//! substitutable, not the guard.
//!
//! ## Narrow by design
//!
//! There is deliberately **no** god trait. The port is split by guarded resource, and a consumer
//! names only the traits it uses — materializing a `secret:env/KEY` credential reference needs
//! [`GuardedEnv`] and nothing else. Where a consumer genuinely spans families it declares its own
//! bundle (see `flux_plugin::PluginSystem`), which keeps the required surface visible at the consumer
//! rather than hidden behind a catch-all.
//!
//! ## What stays inherent on `System`, and why
//!
//! The port covers the operations a substrate can meaningfully *re-implement*. These stay inherent
//! because they are native-only by construction, and putting them on the port would let an
//! implementation claim a posture it cannot hold:
//!
//! - [`System::rerooted`] returns `Self` — a substrate re-roots by handing out a
//!   differently-configured port, not by cloning this one.
//! - [`System::workspace`] / [`System::sandbox`] expose native path resolution and the OS sandbox
//!   posture. A substrate with no filesystem and no bubblewrap has neither.
//! - `run_with_env_exempt` / `run_with_env_streamed*` are about *exempting a child from the OS
//!   sandbox*, which has no meaning where there is no OS sandbox.
//! - `spawn_interactive` / `spawn_debug_pipe` hand back a tty and a POSIX fd pair.
//!
//! The workspace-confined file surface (`read_file`, `write_file`, …) is **not yet a port** — see
//! C-269's story notes. Its consumers all still hold a concrete `System`, and a trait with no call
//! sites would be indirection without a seam.
//!
//! ## These traits are unsealed, and the gate on them stops at this repo
//!
//! Both facts are deliberate, and stated here because neither is obvious from the signatures.
//!
//! **Unsealed.** Any crate depending on published `codewandler-flux-system` can implement
//! `GuardedProcess`, `GuardedHostFiles` or `GuardedEnv`. That is the point — an out-of-repo Wasm
//! embedder serving these ports is the whole reason they exist. It is also not an escalation: a
//! downstream crate that wanted to run an unguarded process could always just call `Command::new`
//! itself. What these traits are is a *contract*, not a permission — implementing one grants no
//! ability, it only claims to uphold the guarantees documented on each method.
//!
//! **The gate is in-repo only.** `flux-codegate`'s `no_unreviewed_guarded_port_backend_outside_system`
//! enumerates every production `impl` of these traits — resolving renamed imports, and excusing only
//! `#[cfg(test)]` — so a second backend cannot appear inside flux without a reviewed allowance. Its
//! reach ends at this repo's two workspaces: it walks `crates/*/src` and `plugins/*/src` and nothing
//! else. It says nothing about downstream implementors, and (like every AST scanner in that file,
//! including the older `Command` gate it mirrors) it does not see macro-generated impls or sources
//! pulled in from outside `src/` via `#[path]`.
//!
//! So: inside flux, "one guarded path starts every OS process" is mechanically enforced. Outside
//! flux, a consumer that implements these traits is taking responsibility for the guarantees itself.
//!
//! ## Fail-closed defaults
//!
//! Optional port operations default to a denial, never to a weaker equivalent. Bringing a substrate
//! up therefore starts from "serves nothing", and each capability is added deliberately — the same
//! deny-by-default posture the plugin host takes with manifest grants.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

pub use flux_core::{Error, Result};

use crate::{ManagedChild, OutputObserver, ProcessOutput, ScopedFileRead, System};

/// The future a guarded port operation returns.
///
/// Boxed rather than `async fn` in a trait: the port has to be usable as `dyn` (a `SystemSource`
/// hands out one erased substrate and consumers store it in an `Arc`), and `async fn` in trait is not
/// yet dyn-compatible. Hand-rolling the box keeps `flux-system`'s dependency set at
/// `flux-core` + `tokio` + `url`, which matters for a crate the portable core has to compile.
pub type Guarded<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

/// Read the process environment through the guarded boundary.
///
/// The narrowest port there is, and the one most consumers want alone: resolving a `secret:env/KEY`
/// reference is an environment read and nothing more. A substrate with no process environment returns
/// `None` for every key, which fails the credential closed.
pub trait GuardedEnv: Send + Sync {
    /// The value of `key` in the guarded environment, or `None` if unset.
    fn env(&self, key: &str) -> Option<String>;
}

/// Start OS processes through the one guarded path: **argv-only** (never a shell string),
/// workspace-pinned cwd, environment cleared to a minimal non-secret allow-list, captured output
/// byte-capped.
///
/// [`run_with_env`](Self::run_with_env) is the only required operation, and the defaults below reduce
/// to it exactly the way [`System`]'s own convenience methods reduce to one private confinement
/// helper — so a substrate implements one primitive and inherits the same delegation graph the native
/// backend has.
pub trait GuardedProcess: Send + Sync {
    /// Execute `argv` (no shell), additionally setting the caller-chosen `env` entries on top of the
    /// minimal allow-list. `env` is built by Rust callers only — model input never reaches it.
    fn run_with_env<'a>(
        &'a self,
        argv: &'a [String],
        env: &'a [(String, String)],
        timeout: Duration,
    ) -> Guarded<'a, ProcessOutput>;

    /// Execute `argv` with only the minimal allow-listed environment.
    fn run<'a>(&'a self, argv: &'a [String], timeout: Duration) -> Guarded<'a, ProcessOutput> {
        self.run_with_env(argv, &[], timeout)
    }

    /// [`run_with_env`](Self::run_with_env) with a live line observer.
    ///
    /// The observer is a *view onto* the captured output, never a second channel, so the default —
    /// run without it — returns a byte-identical result and loses only progress reporting. That is a
    /// safe degradation; a substrate that can stream lines overrides this.
    fn run_with_env_observed<'a>(
        &'a self,
        argv: &'a [String],
        env: &'a [(String, String)],
        timeout: Duration,
        observer: OutputObserver,
    ) -> Guarded<'a, ProcessOutput> {
        let _ = observer;
        self.run_with_env(argv, env, timeout)
    }

    /// [`run`](Self::run) with a live line observer.
    fn run_observed<'a>(
        &'a self,
        argv: &'a [String],
        timeout: Duration,
        observer: OutputObserver,
    ) -> Guarded<'a, ProcessOutput> {
        self.run_with_env_observed(argv, &[], timeout, observer)
    }

    /// Execute `argv`, feeding `stdin` to the child and then closing it.
    ///
    /// Fail-closed default: silently dropping the payload would let a `git apply -` report success
    /// over an empty patch, so a substrate that cannot write a child's stdin must refuse.
    fn run_with_stdin<'a>(
        &'a self,
        argv: &'a [String],
        stdin: &'a [u8],
        timeout: Duration,
    ) -> Guarded<'a, ProcessOutput> {
        let _ = (argv, stdin, timeout);
        Box::pin(async {
            Err(Error::Other(
                "this guarded substrate cannot feed a child process stdin".into(),
            ))
        })
    }

    /// Start a long-lived child and hand back its live handle, for a process started in one call and
    /// queried or stopped in a later one.
    ///
    /// The one port operation whose *result* is irreducibly native — [`ManagedChild`] owns a real
    /// `tokio::process::Child`. A substrate with no OS processes cannot construct one, so it leaves
    /// this default and denies rather than pretending to have spawned something.
    fn spawn_background(&self, argv: &[String], env: &[(String, String)]) -> Result<ManagedChild> {
        let _ = (argv, env);
        Err(Error::Other(
            "this guarded substrate cannot host long-lived child processes".into(),
        ))
    }
}

/// Read files that legitimately live **outside** the workspace jail, admitted by an explicit path
/// scope rather than by workspace confinement.
///
/// This is the seam behind the plugin `fs.read` capability: a `~/.kube/config` is not
/// workspace-relative, so the guard is a declared exact / `/*` / `/**` scope instead. Both the scope
/// anchor and the requested path are reduced to physical identities before matching, so an in-scope
/// symlink spelling cannot name an out-of-scope target. Both operations are required: a substrate that
/// cannot reduce a path to an identity cannot be trusted to match a scope at all, and a fail-open
/// default here would be a security hole rather than a missing feature.
pub trait GuardedHostFiles: Send + Sync {
    /// Reduce a host path to its physical identity — every existing symlink followed, a
    /// not-yet-existing tail preserved. Neither confines to nor widens the workspace; matching the
    /// result against a grant is the caller's job.
    fn host_path_identity(&self, path: &str) -> Result<String>;

    /// Read a host file through an explicit `scope`, capped at `max_bytes`.
    fn read_file_scoped<'a>(
        &'a self,
        path: &'a str,
        scope: &'a str,
        max_bytes: usize,
    ) -> Guarded<'a, ScopedFileRead>;
}

// ---------------------------------------------------------------------------
// The native implementor
// ---------------------------------------------------------------------------
//
// Pure delegation to the inherent methods. Inherent methods win method resolution, so every existing
// `system.run(..)` call site on a concrete `System` still calls the inherent `async fn` and is
// unaffected by these impls.

impl GuardedEnv for System {
    fn env(&self, key: &str) -> Option<String> {
        System::env(self, key)
    }
}

impl GuardedProcess for System {
    fn run_with_env<'a>(
        &'a self,
        argv: &'a [String],
        env: &'a [(String, String)],
        timeout: Duration,
    ) -> Guarded<'a, ProcessOutput> {
        Box::pin(System::run_with_env(self, argv, env, timeout))
    }

    fn run<'a>(&'a self, argv: &'a [String], timeout: Duration) -> Guarded<'a, ProcessOutput> {
        Box::pin(System::run(self, argv, timeout))
    }

    fn run_with_env_observed<'a>(
        &'a self,
        argv: &'a [String],
        env: &'a [(String, String)],
        timeout: Duration,
        observer: OutputObserver,
    ) -> Guarded<'a, ProcessOutput> {
        Box::pin(System::run_with_env_observed(
            self, argv, env, timeout, observer,
        ))
    }

    fn run_observed<'a>(
        &'a self,
        argv: &'a [String],
        timeout: Duration,
        observer: OutputObserver,
    ) -> Guarded<'a, ProcessOutput> {
        Box::pin(System::run_observed(self, argv, timeout, observer))
    }

    fn run_with_stdin<'a>(
        &'a self,
        argv: &'a [String],
        stdin: &'a [u8],
        timeout: Duration,
    ) -> Guarded<'a, ProcessOutput> {
        Box::pin(System::run_with_stdin(self, argv, stdin, timeout))
    }

    fn spawn_background(&self, argv: &[String], env: &[(String, String)]) -> Result<ManagedChild> {
        System::spawn_background(self, argv, env)
    }
}

impl GuardedHostFiles for System {
    fn host_path_identity(&self, path: &str) -> Result<String> {
        System::host_path_identity(self, path)
    }

    fn read_file_scoped<'a>(
        &'a self,
        path: &'a str,
        scope: &'a str,
        max_bytes: usize,
    ) -> Guarded<'a, ScopedFileRead> {
        Box::pin(System::read_file_scoped(self, path, scope, max_bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Workspace;

    /// The native backend reaches the guarded exec path *through the port*, not only through its
    /// inherent methods — so an erased `dyn GuardedProcess` is a usable substitute for `&System`.
    #[tokio::test]
    async fn the_native_system_serves_the_process_port_as_a_trait_object() {
        let system = System::new(Workspace::new(std::env::temp_dir()).unwrap());
        let port: &dyn GuardedProcess = &system;
        let argv = vec!["echo".to_string(), "port".to_string()];

        let out = port.run(&argv, Duration::from_secs(30)).await.unwrap();

        assert_eq!(out.exit_code, 0);
        assert_eq!(out.stdout.trim(), "port");
    }

    /// A substrate that implements only the required primitive still answers every derived operation,
    /// and the ones it cannot serve deny instead of degrading.
    #[tokio::test]
    async fn a_minimal_substrate_inherits_the_delegations_and_denies_the_rest() {
        struct Minimal;

        impl GuardedProcess for Minimal {
            fn run_with_env<'a>(
                &'a self,
                argv: &'a [String],
                env: &'a [(String, String)],
                _timeout: Duration,
            ) -> Guarded<'a, ProcessOutput> {
                let stdout = format!("{}|{}", argv.join(" "), env.len());
                Box::pin(async move {
                    Ok(ProcessOutput {
                        stdout,
                        stderr: String::new(),
                        exit_code: 0,
                    })
                })
            }
        }

        let argv = vec!["true".to_string()];
        let port: &dyn GuardedProcess = &Minimal;

        assert_eq!(
            port.run(&argv, Duration::from_secs(1))
                .await
                .unwrap()
                .stdout,
            "true|0",
            "`run` must reduce to the required `run_with_env` with an empty env"
        );

        let observer = std::sync::Arc::new(|_line: &str| {}) as OutputObserver;
        assert_eq!(
            port.run_observed(&argv, Duration::from_secs(1), observer)
                .await
                .unwrap()
                .stdout,
            "true|0",
            "the observer default must not change the captured result"
        );

        let stdin_error = port
            .run_with_stdin(&argv, b"patch", Duration::from_secs(1))
            .await
            .expect_err("stdin must fail closed, never silently drop the payload");
        assert!(stdin_error
            .to_string()
            .contains("cannot feed a child process"));

        // `ManagedChild` is not `Debug` (it owns a live child), so match rather than `expect_err`.
        let spawn_error = match port.spawn_background(&argv, &[]) {
            Err(error) => error,
            Ok(_) => panic!("a long-lived native child cannot be fabricated"),
        };
        assert!(spawn_error
            .to_string()
            .contains("cannot host long-lived child processes"));
    }
}
