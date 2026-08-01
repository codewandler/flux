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
//! The workspace-confined file surface **is** a port — [`GuardedWorkspaceFiles`] (C-395). C-269
//! deferred it on the stated grounds that every consumer held a concrete `System`, so a trait with
//! no call sites would be indirection without a seam. A second consumer of the substrate is exactly
//! the condition that expired that reasoning. What stays inherent from *that* family is the
//! atomic-replacement pair: `write_file_atomic` and `update_file_reserved` are contracts over
//! filesystem primitives (`O_EXCL` plus a same-directory `rename`) rather than over file contents,
//! and `update_file_reserved` takes a caller closure, which is not dyn-compatible at all.
//!
//! ## These traits are unsealed, and the gate on them stops at this repo
//!
//! Both facts are deliberate, and stated here because neither is obvious from the signatures.
//!
//! **Unsealed.** Any crate depending on published `codewandler-flux-system` can implement
//! `GuardedProcess`, `GuardedHostFiles`, `GuardedWorkspaceFiles` or `GuardedEnv`. That is the point
//! — an out-of-repo Wasm embedder serving these ports is the whole reason they exist. It is also not
//! an escalation: a downstream crate that wanted to run an unguarded process could always just call
//! `Command::new` itself. What these traits are is a *contract*, not a permission — implementing one
//! grants no ability, it only claims to uphold the guarantees documented on each method.
//!
//! **The gate is in-repo only, and it enumerates three of the four ports.** `flux-codegate`'s
//! `no_unreviewed_guarded_port_backend_outside_system` reports every production `impl` of
//! [`GuardedProcess`], [`GuardedHostFiles`] and [`GuardedEnv`] — resolving renamed imports, and
//! excusing only `#[cfg(test)]` — so a second backend for one of those cannot appear inside flux
//! without a reviewed allowance. Its reach ends at this repo's two workspaces: it walks
//! `crates/*/src` and `plugins/*/src` and nothing else. It says nothing about downstream
//! implementors, and (like every AST scanner in that file, including the older `Command` gate it
//! mirrors) it does not see macro-generated impls or sources pulled in from outside `src/` via
//! `#[path]`.
//!
//! [`GuardedWorkspaceFiles`] is **not** in that enumeration. C-395's acceptance required the port to
//! land without adding an entry to the gate's allow-list, and adding the trait to
//! `GUARDED_PORT_TRAITS` would have forced exactly that for the native delegation below. So the one
//! in-repo implementor is the reviewed one by construction rather than by check, and a second
//! in-repo workspace-file backend would land unremarked. That is a scope call, not a claim that such
//! a backend is harmless: an implementation of this port claims workspace confinement just as
//! literally as a `GuardedProcess` implementation claims argv-only spawning. Closing the gap is two
//! lines — the trait name in `GUARDED_PORT_TRAITS` and the `(port.rs, GuardedWorkspaceFiles, System)`
//! allowance beside its three siblings.
//!
//! So: inside flux, "one guarded path starts every OS process" is mechanically enforced. Outside
//! flux, a consumer that implements these traits is taking responsibility for the guarantees itself.
//!
//! That is the *implementor's* question. The adjacent one — what a consumer that merely **links**
//! `flux-system`, native backend and all, does and does not inherit by doing so — is answered once
//! at the crate root, under "Binding `flux-system` without `flux-runtime`". Neither answer is
//! repeated here.
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

/// Read and write files **inside the workspace jail** — the counterpart to [`GuardedHostFiles`],
/// which is for the files that legitimately live outside it.
///
/// Every path is resolved against the implementor's workspace before any open, and both escape
/// shapes are refused: a lexical `..` that normalizes out of the matched root, and a symlink whose
/// target canonicalizes outside it (including a *dangling* one, which a plain parent-canonicalize
/// misses on write).
///
/// # The asymmetry is part of the contract
///
/// Reads and writes are **not** confined to the same set. A read resolves against the primary root,
/// any `@named` root, **and** any configured read-only root; a write resolves against the primary
/// and `@named` roots only. That is the whole point of a read root, and it is why the two primitives
/// below are separate required methods rather than one `access: PathAccess` parameter with a
/// caller-chosen argument: an implementor cannot serve this port while collapsing the two, and a
/// consumer holding only the trait cannot ask for a write "as a read".
///
/// Both directions are required for the same reason both of [`GuardedHostFiles`]'s operations are:
/// a substrate that answers reads but silently inherits a generic denial for writes is
/// indistinguishable from one whose write guard was never wired, and the difference matters.
/// A genuinely read-only substrate says so in its own words, in its own `write_file_bytes`.
pub trait GuardedWorkspaceFiles: Send + Sync {
    /// The raw bytes of a workspace file — no UTF-8 decode, so a caller can sniff binary content
    /// and report byte sizes before a lossy text decode. Resolved on the **read** path, so a
    /// configured read-only root is reachable.
    fn read_file_bytes<'a>(&'a self, path: &'a str) -> Guarded<'a, Vec<u8>>;

    /// Write raw bytes to a workspace file, creating parent directories (also confined). Resolved
    /// on the **write** path, so a read-only root is *not* reachable.
    fn write_file_bytes<'a>(&'a self, path: &'a str, contents: &'a [u8]) -> Guarded<'a, ()>;

    /// A workspace file decoded as UTF-8.
    ///
    /// Reduces to [`read_file_bytes`](Self::read_file_bytes) exactly the way the native backend's
    /// own `read_file` does — same guard, same decode, same error — so the default is the native
    /// behaviour rather than an approximation of it.
    fn read_file<'a>(&'a self, path: &'a str) -> Guarded<'a, String> {
        Box::pin(async move {
            let bytes = self.read_file_bytes(path).await?;
            String::from_utf8(bytes).map_err(|_| Error::Other(format!("{path}: not valid UTF-8")))
        })
    }

    /// Write UTF-8 text to a workspace file, creating parent directories (also confined).
    ///
    /// Reduces to [`write_file_bytes`](Self::write_file_bytes): the native backend's `write_file`
    /// and `write_file_bytes` differ only in the type of the payload they hand to the same guarded
    /// write, so the reduction is byte-identical rather than merely equivalent.
    fn write_file<'a>(&'a self, path: &'a str, contents: &'a str) -> Guarded<'a, ()> {
        self.write_file_bytes(path, contents.as_bytes())
    }

    /// Append text to a workspace file, creating it (and parent directories) if absent.
    ///
    /// Fail-closed default. Read-then-write is *not* an append: it races every other writer and
    /// turns a crash mid-write into a truncation of content the caller never intended to touch. A
    /// substrate that cannot open a file for append refuses rather than emulating one.
    fn append_file<'a>(&'a self, path: &'a str, contents: &'a str) -> Guarded<'a, ()> {
        let _ = (path, contents);
        Box::pin(async { Err(deny("append to a workspace file")) })
    }

    /// At most `max` bytes of a **regular** workspace file, as `(bytes, truncated)`.
    ///
    /// Fail-closed default. Reducing this to [`read_file_bytes`](Self::read_file_bytes) and
    /// truncating afterwards would lose both guarantees that make it worth having — the memory bound
    /// on an attacker-influenced size, and the refusal of a FIFO or device that would otherwise
    /// stream forever — so the degradation is not safe and the operation denies instead.
    fn read_file_bytes_capped<'a>(
        &'a self,
        path: &'a str,
        max: usize,
    ) -> Guarded<'a, (Vec<u8>, bool)> {
        let _ = (path, max);
        Box::pin(async { Err(deny("read a workspace file under a byte cap")) })
    }

    /// The byte size of a workspace file, as a metadata call.
    ///
    /// Fail-closed default: the point of asking is to skip an oversized file *without* reading it,
    /// so answering by reading it would invert the operation's reason to exist.
    fn file_size<'a>(&'a self, path: &'a str) -> Guarded<'a, u64> {
        let _ = path;
        Box::pin(async { Err(deny("stat a workspace file's size")) })
    }

    /// Whether a path exists inside the workspace or its read roots.
    ///
    /// Fail-closed default. `Ok(false)` would be the dangerous answer, not the neutral one: a
    /// caller asks this to decide whether it is creating or overwriting, and a substrate that
    /// cannot tell must not report "nothing there".
    fn path_exists<'a>(&'a self, path: &'a str) -> Guarded<'a, bool> {
        let _ = path;
        Box::pin(async { Err(deny("test whether a workspace path exists")) })
    }

    /// Whether a workspace path is a directory. Fail-closed for the same reason as
    /// [`path_exists`](Self::path_exists): a guessed `false` sends the caller down the file path.
    fn is_dir<'a>(&'a self, path: &'a str) -> Guarded<'a, bool> {
        let _ = path;
        Box::pin(async { Err(deny("test whether a workspace path is a directory")) })
    }

    /// The last-modification time of a workspace file.
    ///
    /// Fail-closed default: this is what a read-before-write guard compares to detect a file that
    /// changed under the caller, so a fabricated timestamp would defeat the guard rather than
    /// degrade it.
    fn file_mtime<'a>(&'a self, path: &'a str) -> Guarded<'a, std::time::SystemTime> {
        let _ = path;
        Box::pin(async { Err(deny("read a workspace file's modification time")) })
    }

    /// The entry names of a workspace directory, sorted.
    ///
    /// Fail-closed default: an empty `Vec` is a *wrong answer* ("the directory is empty"), not a
    /// missing feature, and callers act on it.
    fn list_dir<'a>(&'a self, path: &'a str) -> Guarded<'a, Vec<String>> {
        let _ = path;
        Box::pin(async { Err(deny("list a workspace directory")) })
    }

    /// Files under a workspace directory, recursively, capped at `max`. Symlinks are never
    /// followed. Fail-closed for the same reason as [`list_dir`](Self::list_dir).
    fn walk_files<'a>(&'a self, base: &'a str, max: usize) -> Guarded<'a, Vec<String>> {
        let _ = (base, max);
        Box::pin(async { Err(deny("walk a workspace directory")) })
    }
}

/// The refusal an unserved optional port operation returns. One spelling, so a consumer can tell a
/// capability the substrate does not offer from a guard that rejected its path.
fn deny(operation: &str) -> Error {
    Error::Other(format!("this guarded substrate cannot {operation}"))
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

// Every operation, including the two the trait would otherwise default, so the native backend
// answers each one through its own guarded method rather than through a reduction — the port and the
// struct are then the same code path, not merely the same outcome.
impl GuardedWorkspaceFiles for System {
    fn read_file_bytes<'a>(&'a self, path: &'a str) -> Guarded<'a, Vec<u8>> {
        Box::pin(System::read_file_bytes(self, path))
    }

    fn write_file_bytes<'a>(&'a self, path: &'a str, contents: &'a [u8]) -> Guarded<'a, ()> {
        Box::pin(System::write_file_bytes(self, path, contents))
    }

    fn read_file<'a>(&'a self, path: &'a str) -> Guarded<'a, String> {
        Box::pin(System::read_file(self, path))
    }

    fn write_file<'a>(&'a self, path: &'a str, contents: &'a str) -> Guarded<'a, ()> {
        Box::pin(System::write_file(self, path, contents))
    }

    fn append_file<'a>(&'a self, path: &'a str, contents: &'a str) -> Guarded<'a, ()> {
        Box::pin(System::append_file(self, path, contents))
    }

    fn read_file_bytes_capped<'a>(
        &'a self,
        path: &'a str,
        max: usize,
    ) -> Guarded<'a, (Vec<u8>, bool)> {
        Box::pin(System::read_file_bytes_capped(self, path, max))
    }

    fn file_size<'a>(&'a self, path: &'a str) -> Guarded<'a, u64> {
        Box::pin(System::file_size(self, path))
    }

    fn path_exists<'a>(&'a self, path: &'a str) -> Guarded<'a, bool> {
        Box::pin(System::path_exists(self, path))
    }

    fn is_dir<'a>(&'a self, path: &'a str) -> Guarded<'a, bool> {
        Box::pin(System::is_dir(self, path))
    }

    fn file_mtime<'a>(&'a self, path: &'a str) -> Guarded<'a, std::time::SystemTime> {
        Box::pin(System::file_mtime(self, path))
    }

    fn list_dir<'a>(&'a self, path: &'a str) -> Guarded<'a, Vec<String>> {
        Box::pin(System::list_dir(self, path))
    }

    fn walk_files<'a>(&'a self, base: &'a str, max: usize) -> Guarded<'a, Vec<String>> {
        Box::pin(System::walk_files(self, base, max))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{sandbox, Workspace};
    use std::path::PathBuf;

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

    /// A workspace root, plus a sibling directory *outside* it holding `secret.txt`, plus a symlink
    /// inside the root pointing at that sibling. Returns the root, the outside directory, and the
    /// relative spelling that lexically escapes the root.
    fn escape_fixture(prefix: &str) -> (PathBuf, PathBuf, String, System) {
        let root = sandbox::fixture_dir(prefix);
        let outside = sandbox::fixture_dir(&format!("{prefix}-outside"));
        std::fs::write(outside.join("secret.txt"), "outside").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();

        // Both fixture roots are siblings under the temp dir, so `..` reaches one from the other.
        let lexical = format!(
            "../{}/secret.txt",
            outside.file_name().unwrap().to_string_lossy()
        );
        let system = System::new(Workspace::new(&root).unwrap());
        (root, outside, lexical, system)
    }

    /// C-395 — the workspace jail travels with the *port*, not only with the struct.
    ///
    /// A consumer holding nothing but `&dyn GuardedWorkspaceFiles` is refused both escape shapes
    /// that a concrete-`System` consumer is refused: a lexical `..` out of the root, and a symlink
    /// whose target canonicalizes outside it. Each refusal is checked twice — once through the
    /// trait and once through the struct — so the test fails if the port's confinement ever drifts
    /// from the inherent method's, in either direction. And a refusal is only a refusal if nothing
    /// happened: the outside directory is inspected afterwards.
    #[tokio::test]
    async fn the_file_port_refuses_the_escapes_the_concrete_system_refuses() {
        let (root, outside, lexical, system) = escape_fixture("port-file-escape");
        let port: &dyn GuardedWorkspaceFiles = &system;
        let through_link = "link/secret.txt";

        for path in [lexical.as_str(), through_link] {
            assert!(
                port.read_file(path).await.is_err(),
                "the port must refuse a read of {path:?}"
            );
            assert!(
                System::read_file(&system, path).await.is_err(),
                "the struct must refuse a read of {path:?} — the port is not stricter than it"
            );
            assert!(
                port.read_file_bytes(path).await.is_err(),
                "the port must refuse a byte read of {path:?}"
            );
            assert!(
                port.write_file(path, "owned").await.is_err(),
                "the port must refuse a write of {path:?}"
            );
            assert!(
                System::write_file(&system, path, "owned").await.is_err(),
                "the struct must refuse a write of {path:?}"
            );
            assert!(
                port.write_file_bytes(path, b"owned").await.is_err(),
                "the port must refuse a byte write of {path:?}"
            );
        }

        assert_eq!(
            std::fs::read_to_string(outside.join("secret.txt")).unwrap(),
            "outside",
            "a refused write through the port still reached the outside file"
        );
        assert_eq!(
            std::fs::read_dir(&outside).unwrap().count(),
            1,
            "a refused write through the port created a file outside the workspace"
        );

        // The port is confined, not broken: an in-root path round-trips through it.
        port.write_file("inside.txt", "kept").await.unwrap();
        assert_eq!(port.read_file("inside.txt").await.unwrap(), "kept");

        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    /// C-395 — the native backend answers *every* file operation through the port, including the
    /// optional ones. Without this the delegation could quietly lose an override and inherit the
    /// trait's denial, which would look like a working port right up until a consumer asked.
    #[tokio::test]
    async fn the_native_system_serves_every_file_operation_through_the_port() {
        let root = sandbox::fixture_dir("port-file-native");
        let system = System::new(Workspace::new(&root).unwrap());
        let port: &dyn GuardedWorkspaceFiles = &system;

        port.write_file("dir/a.txt", "hello").await.unwrap();
        port.append_file("dir/a.txt", " again").await.unwrap();

        assert_eq!(port.read_file("dir/a.txt").await.unwrap(), "hello again");
        assert_eq!(port.file_size("dir/a.txt").await.unwrap(), 11);
        assert_eq!(
            port.read_file_bytes_capped("dir/a.txt", 5).await.unwrap(),
            (b"hello".to_vec(), true)
        );
        assert!(port.path_exists("dir/a.txt").await.unwrap());
        assert!(!port.path_exists("dir/missing.txt").await.unwrap());
        assert!(port.is_dir("dir").await.unwrap());
        assert!(!port.is_dir("dir/a.txt").await.unwrap());
        port.file_mtime("dir/a.txt").await.unwrap();
        assert_eq!(port.list_dir("dir").await.unwrap(), vec!["a.txt"]);
        assert_eq!(port.walk_files(".", 100).await.unwrap(), vec!["dir/a.txt"]);

        std::fs::remove_dir_all(&root).ok();
    }

    /// C-395 — the read/write asymmetry survives the port.
    ///
    /// A `read_root` (C-21) widens *reads* only. Through the trait, exactly as through the struct, a
    /// file under one is readable and the same directory is not writable — so a consumer that swaps
    /// a concrete `System` for the port cannot quietly gain a write it did not have.
    #[tokio::test]
    async fn read_roots_stay_readable_and_unwritable_through_the_file_port() {
        let root = sandbox::fixture_dir("port-file-readroot");
        let read_root = sandbox::fixture_dir("port-file-readroot-extra");
        std::fs::write(read_root.join("notes.md"), "readable").unwrap();

        let mut workspace = Workspace::new(&root).unwrap();
        workspace.add_read_root(&read_root).unwrap();
        let system = System::new(workspace);
        let port: &dyn GuardedWorkspaceFiles = &system;

        let readable = read_root.join("notes.md").to_string_lossy().into_owned();
        assert_eq!(
            port.read_file(&readable).await.unwrap(),
            "readable",
            "a read root must be readable through the port"
        );

        let unwritable = read_root.join("planted.md").to_string_lossy().into_owned();
        assert!(
            port.write_file(&unwritable, "planted").await.is_err(),
            "a read root must NOT be writable through the port"
        );
        assert!(
            System::write_file(&system, &unwritable, "planted")
                .await
                .is_err(),
            "a read root must NOT be writable through the struct either"
        );
        assert!(
            port.write_file(&readable, "overwritten").await.is_err(),
            "an existing read-root file must not be overwritable through the port"
        );
        assert_eq!(
            std::fs::read_to_string(read_root.join("notes.md")).unwrap(),
            "readable",
            "a refused overwrite through the port still modified the read-root file"
        );
        assert!(
            !read_root.join("planted.md").exists(),
            "a refused write through the port still created a file in the read root"
        );

        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&read_root).ok();
    }

    /// A substrate implementing only the two required primitives still answers the text operations,
    /// and every optional one denies rather than degrading to a weaker equivalent.
    #[tokio::test]
    async fn a_minimal_file_substrate_inherits_the_text_reductions_and_denies_the_rest() {
        #[derive(Default)]
        struct Memory(std::sync::Mutex<Vec<(String, Vec<u8>)>>);

        impl GuardedWorkspaceFiles for Memory {
            fn read_file_bytes<'a>(&'a self, path: &'a str) -> Guarded<'a, Vec<u8>> {
                Box::pin(async move {
                    self.0
                        .lock()
                        .unwrap()
                        .iter()
                        .find(|(name, _)| name == path)
                        .map(|(_, bytes)| bytes.clone())
                        .ok_or_else(|| Error::Other(format!("{path}: not found")))
                })
            }

            fn write_file_bytes<'a>(
                &'a self,
                path: &'a str,
                contents: &'a [u8],
            ) -> Guarded<'a, ()> {
                Box::pin(async move {
                    self.0
                        .lock()
                        .unwrap()
                        .push((path.to_string(), contents.to_vec()));
                    Ok(())
                })
            }
        }

        let memory = Memory::default();
        let port: &dyn GuardedWorkspaceFiles = &memory;

        port.write_file("note.md", "text").await.unwrap();
        assert_eq!(
            port.read_file_bytes("note.md").await.unwrap(),
            b"text".to_vec(),
            "`write_file` must reduce to the required `write_file_bytes` byte-for-byte"
        );
        assert_eq!(
            port.read_file("note.md").await.unwrap(),
            "text",
            "`read_file` must reduce to the required `read_file_bytes`"
        );

        port.write_file_bytes("raw.bin", &[0xff, 0xfe])
            .await
            .unwrap();
        let decode_error = port
            .read_file("raw.bin")
            .await
            .expect_err("the text reduction must not decode non-UTF-8 lossily");
        assert!(decode_error.to_string().contains("not valid UTF-8"));

        // Every optional operation denies. `path_exists`/`is_dir` are the ones a well-meaning
        // default would answer `false` to, which is why they are asserted alongside the rest.
        for (label, result) in [
            ("append", port.append_file("note.md", "more").await.err()),
            (
                "capped read",
                port.read_file_bytes_capped("note.md", 2).await.err(),
            ),
            ("size", port.file_size("note.md").await.err()),
            ("exists", port.path_exists("note.md").await.err()),
            ("is_dir", port.is_dir("note.md").await.err()),
            ("mtime", port.file_mtime("note.md").await.err()),
            ("list", port.list_dir(".").await.err()),
            ("walk", port.walk_files(".", 10).await.err()),
        ] {
            let error = result.unwrap_or_else(|| {
                panic!("the optional `{label}` operation must fail closed, not degrade")
            });
            assert!(
                error
                    .to_string()
                    .starts_with("this guarded substrate cannot"),
                "`{label}` denied with an off-contract message: {error}"
            );
        }
    }
}
