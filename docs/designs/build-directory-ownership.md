# Build-directory ownership

`task install` and every repository-owned Cargo entry point that can touch a shared Flux target use
one cross-process ownership contract. The source of truth is `scripts/build_ownership.py`; the
checked `scripts/test_build_entrypoints.py` inventory rejects a new unowned spelling.

## Contract

1. Resolve the workspace root once. An unset `CARGO_TARGET_DIR` means `<workspace>/target`; an
   absolute override stays absolute; a relative override is resolved against the workspace root,
   which is also the child Cargo process's working directory. The child receives that resolved
   absolute path, so nested Task and Cargo invocations cannot reinterpret it. Lock identity also
   resolves an existing target symlink/junction and existing symlinked parents: two path spellings
   for one physical target therefore open the same physical sibling lock.
2. The ownership file is a stable sibling of the target (`<target>.flux-build.lock`), never inside
   the directory `cargo clean` removes. Builders take a shared advisory OS lock before Cargo can
   create or consume anything below the target and retain it until the complete governed process
   tree has exited. Cleanup takes the exclusive form of the same lock before it runs.
3. Shared acquisition waits and says which target it is waiting for. Repository cleanup is
   deliberately non-blocking: it refuses with exit 75, names the governed target, and tells the
   operator to retry after active builds finish. A failed build retains its own exit status and is
   diagnosed separately from refusal to acquire ownership.
4. The lock file is persistent and bounded at one small file per physical selected target. The OS
   releases the lock on success, Cargo or native-compiler failure, cancellation, and abnormal
   wrapper/child exit. Recovery never reaps by PID, age, Git state or a process snapshot, and the
   lock file is never unlinked, avoiding the Unix split-lock race where a new inode could be locked
   while an old owner is still live.

The implementation uses `flock(2)` semantics on Unix (Linux and macOS) and `LockFileEx` on Windows.
On Unix, the governed tree inherits the same locked open-file description plus a lifetime pipe. A
healthy wrapper observes pipe EOF before release; if the wrapper is killed, the kernel retains the
shared lease until the last target-touching descendant closes its inherited description. On
Windows, the wrapper creates the governed command suspended, assigns it to a kill-on-close Job
Object, then resumes it. An I/O completion port proves active-process-zero before healthy release.
`STARTUPINFOEX` restricts inheritance to duplicated standard handles; unrelated inheritable parent
handles are excluded. If the wrapper is terminated, Job closure kills its process tree; the native
regression queues an exclusive waiter first and makes its first governed operation verify that the
descendant process and a no-share target handle are already closed.

The protocol is advisory by design: direct operator Cargo commands retain Cargo's ordinary reusable
build cache, but every repository script which shares a target uses the wrapper, and
repository-owned cleanup is only `task clean`. An external cleanup of the fleet's inherited
per-repository `CARGO_TARGET_DIR` must either use that exclusive entry point or preserve the target.
The fleet's stronger refusal to clean primary ignored caches remains valid and unchanged.

## Pre-Cargo bootstrap and entry-point inventory

The lease implementation is a checked-in Python 3.10+ standard-library program, not a Rust helper.
It therefore opens the sibling lock and acquires ownership before Cargo exists in the process tree;
freshness of `CARGO_HOME` cannot affect acquisition. `scripts/run-python3.sh` selects `python3` then
`python` on Linux/macOS. `scripts/run-python3.cmd` selects `python` then `py -3` on Windows. Both
validate the minimum version before launching and return exit 69 with one fixed install/override
action when no supported runtime exists. `PYTHON=<executable>` is an explicit Task override, not the
default Windows path. The diagnostic never prints the candidate path or an environment value.

The governed inventory is:

| Owner | Target relationship | Enforcement |
|---|---|---|
| `task test`, `task install`, `task clean` | Root workspace target; install holds one shared lease across tests and both installs | Direct Python bootstrap; cleanup is the sole exclusive entry point |
| `task plugins:build`, plugin release build/index/publish | Nested `plugins/` workspace target | Same wrapper with the nested workspace as the resolved root |
| Repository executable/scripts at any depth and build/test/clippy/run/publish calls | Root target, or the explicitly named child-worktree/plugin root | `owned_cargo` / `owned_cargo_at` adapters call the same Python source; recursive inventory covers shell, Python, PowerShell, cmd/batch and alternate Cargo spellings |
| CI, composite actions, assurance and release workflow build commands, including `dist build` | Ephemeral checkout target, reused across steps | `scripts/owned-cargo` or the same wrapper for nested workspaces; workflows with literal `target/...` consumers explicitly pin `CARGO_TARGET_DIR: target` |
| `cargo fmt`, `fetch`, `metadata`, `update`, `upgrade`, `audit`, and `deny` | Do not create or consume compiler output beneath the target | Inventory records them as non-target Cargo operations |

Direct operator Cargo remains supported and retains Cargo's normal cache behavior, but it is not a
repository cleanup actor. Operators who share its target with repository automation must not run
raw removal concurrently; use `task clean`, or wait until every direct build has ended.

## Cache and cleanup boundary

The governed target remains Cargo's reusable target directory; this design creates no invocation
target and no second build cache. `task clean` runs Cargo's own scoped cleanup only after exclusive
ownership is acquired. Before acquiring, it rejects any target that overlaps `CARGO_HOME`, contains
a registered checkout/worktree, or is an existing checkout subtree containing tracked content.
Physical aliases receive the same checks. Existing ignored targets and new workspace-relative
targets remain valid. The persistent sibling lock is not a temporary tree and does not grow across
failures.

`task install` also leaves Cargo's install destination semantics intact. Tests set `HOME`,
`CARGO_HOME`, and `CARGO_INSTALL_ROOT` to test-owned directories; production continues to use the
operator's normal Cargo install root.
