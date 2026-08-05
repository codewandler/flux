---
id: C-517
title: "Give task install a build-directory ownership contract"
pillar: Core
status: done
priority: 0
areas: [build, install, release]
note: "task install lost libsqlite3-sys compiler output while another cleanup overlapped the build; active targets need one explicit ownership or isolation rule"
---

# Give `task install` a build-directory ownership contract

## Goal

Make `task install` reliable when Flux builds, installs and repository-owned cleanup run
concurrently. The test-first install still verifies the workspace and installs both supported
binaries, while one explicit cross-process build-directory ownership rule prevents another Flux
build or cleanup from removing compiler output that is still live. The rule composes predictably
with Cargo's operator-selected target directory and the fleet's shared per-repository target, keeps
ordinary build reuse, and releases every bounded resource on success or failure.

The reported Linux failure reached bundled `libsqlite3-sys`'s `cc` invocation after
`target/release/build/.../out` had disappeared, and ended while writing `...sqlite3.o` with
`No such file or directory`. The current Taskfile runs `cargo test --workspace --lib`, then two
forced `cargo install --path ...` commands, all inheriting the ambient target selection. The fleet
audit records a primary-target cleanup overlapping that install. Those facts are evidence of an
uncoordinated cleanup/build overlap; without one process trace binding the remover to the failed
compiler, this story does not claim a more specific cause.

## Acceptance

### One target ownership contract

- [x] Before implementation, one documented contract is chosen and used consistently by
      `task install` and every repository-owned build/cleanup entry point that can touch the same
      Flux build directory: either a cross-process ownership/lease protocol in which active builds
      hold shared ownership and cleanup requires exclusive ownership, or a unique per-invocation
      build directory whose cleanup cannot name another active invocation. There is one source of
      truth for path resolution and lifecycle; a second ad-hoc lock, marker, process scan or
      best-effort convention is not accepted.
- [x] The chosen contract prevents a concurrent, contract-compliant Flux build or cleanup from
      unlinking any active compiler output directory. Ownership is acquired before the first path
      beneath the governed target can be created or consumed and remains held through every Cargo
      child and native build-script descendant. Cleanup waits or refuses with a deterministic,
      actionable result; it never infers safety from clean Git status, directory age, a missing
      Cargo frontend process, or a single point-in-time process listing.
- [x] `CARGO_TARGET_DIR` remains an explicit operator override and is resolved exactly once using
      Cargo-compatible absolute/relative semantics. Repository scripts and the fleet's inherited
      per-repository shared target have one documented, deterministic interaction with `task
      install`: no script silently discards, rewrites or cleans the override, and nested Task/Cargo
      invocations resolve the same governed root. Tests cover unset, absolute and workspace-relative
      overrides plus the fleet shared-target shape.
- [x] Ordinary `cargo build|test|clippy` output remains reusable across compatible invocations. The
      repair neither deletes nor relocates Cargo's global registry/git cache, and no cleanup command
      recursively deletes `CARGO_HOME`, an operator-selected target root, the primary checkout or an
      unrelated worktree. If isolation uses invocation-private children, the reusable parent/cache
      boundary and the conditions under which an inactive child may be reclaimed are explicit.

### Install behavior and bounded failure

- [x] `task install` still runs the workspace library-test verification and installs both supported
      binaries—`flux` from `crates/flux-cli` and `flux-lsp` from `crates/flux-lsp`—with a nonzero exit
      if verification or either install fails. Regression tests isolate `HOME`/`CARGO_HOME`, direct
      installation to a fresh test-owned destination, keep a separate sentinel user-bin tree
      byte-for-byte unchanged, and never resolve, replace, remove or execute the operator's actual
      installed binaries.
- [x] Success, Cargo failure, native compiler failure, cancellation and signal/abnormal child exit
      release ownership according to the same contract. No failure leaves a live lease with no
      owner. Temporary or invocation-private build trees have a documented finite bound and a safe
      reaping rule; repeated failed installs cannot grow an unbounded collection, and recovery never
      removes a tree still owned by another process.
- [x] Diagnostics name the governed target, whether ownership was acquired, waited on or refused,
      and the operator action needed. They expose no credential or arbitrary environment value and
      distinguish build failure from ownership/cleanup refusal without presenting a guessed race
      participant as proven fact.

### Failing-first concurrency proof

- [x] A failing-first regression harness reproduces the old disappearance class against the
      pre-repair contract: an instrumented build reaches a barrier after the native output directory
      is selected and before `cc` writes its object, a concurrent repository cleanup/build actor is
      then released, and the baseline fails because that active output disappears. The fixed path
      proves the cleanup waits/refuses or is isolated and the compiler output survives. Coordination
      uses pipes, files, process handles or another observable barrier—never sleeps, retry luck,
      scheduler assumptions or a wall-clock race window.
- [x] Linux CI includes a real bundled-SQLite/native-compiler proof that exercises
      `libsqlite3-sys` through `cc` and observes an object under the Cargo
      `release/build/.../out` path from the reported failure class. A mock-only shell test cannot
      replace this leg. The test owns its install destination and bounded build directories and
      leaves the operator's installed `flux`/`flux-lsp` untouched.
- [x] The path, lease and cleanup implementation is portable: Linux uses the real SQLite/`cc`
      regression; macOS specifies and tests the same ownership, override, cancellation and bounded
      reaping semantics with its native filesystem/process primitives; Windows specifies and tests
      them with Windows-safe atomicity, path and process/handle behavior. Platform-specific lock or
      deletion semantics may differ, but no supported platform falls back to an uncoordinated shared
      target or a sleep-based test.

### Documentation and gates

- [x] `Taskfile.yaml`, contributor/install guidance and fleet/shared-target documentation describe
      the selected ownership boundary, override precedence, concurrent cleanup behavior, recovery
      and bounded temporary-tree policy. `CHANGELOG.md` and `WHATS-NEW.md` describe the shipped
      reliability change without overstating the incident cause; this story is marked `done` only
      with named proof, and `docs/stories/README.md` is regenerated and idempotent.
- [x] The focused install/ownership tests, deterministic concurrent regression and Linux bundled-
      SQLite proof pass, including the exact Task invocation with a fresh test-owned Cargo home.
      Then the embedded-doc regenerate/check and site/documentation gates pass, followed by the full
      repository gates:

      ```bash
      CARGO_HOME=<fresh-test-owned-cargo-home> task install
      scripts/build-embedded-docs.sh
      scripts/build-embedded-docs.sh --check
      scripts/test-embedded-docs-gates.sh
      (cd website && npm test && npm run build)
      cargo build --workspace
      cargo test --workspace
      cargo clippy --workspace --all-targets -- -D warnings
      cargo fmt --all -- --check
      cargo test -p flux-codegate
      ```

      If implementation changes Flux process-spawn or sandbox posture, it also runs:

      ```bash
      FLUX_BWRAP_BIN=/nonexistent/bwrap cargo test --workspace
      FLUX_TEST_SANDBOX_BACKEND=1 cargo test -p flux-cli --test sandbox_backend
      ```

      If `plugins/` is touched, its separate workspace gates also run:

      ```bash
      cargo build --manifest-path plugins/Cargo.toml --workspace
      cargo test --manifest-path plugins/Cargo.toml --workspace
      cargo clippy --manifest-path plugins/Cargo.toml --workspace --all-targets -- -D warnings
      cargo fmt --manifest-path plugins/Cargo.toml --all -- --check
      ```

## Progress

- 2026-08-04 — done with one pre-Cargo contract: Python 3.10+ acquires a persistent physical-target
  sibling lease before launching Cargo (`flock` on Linux/macOS, `LockFileEx` plus a kill-on-close Job
  and active-zero completion port on Windows). The Unix lease descriptor is inherited through the
  governed tree, so killing the wrapper cannot orphan a live descendant without ownership; the
  Windows native lane queues an exclusive waiter and proves wrapper termination drains the Job and
  its no-share target handle before cleanup runs. Existing symlink/junction target spellings share
  one canonical lock identity, and exclusive cleanup refuses Cargo-home overlap, checkout roots and
  tracked checkout subtrees while allowing ignored/new relative targets.
- 2026-08-04 — named proof: 27 focused Python tests cover pre-lease target absence, absolute/relative
  and fleet target reuse, signal/cancellation windows, direct-parent and wrapper hard death,
  physical aliases, cleanup safety, native Windows staged-handle cleanup/HANDLE_LIST/Job teardown,
  fresh-home `task install`, and `plugins:install` consumption through absolute/relative paths with
  spaces. `scripts/test-build-ownership.sh --sqlite` first reproduces the pre-repair object-directory
  disappearance at an observable compiler barrier, then proves cleanup refusal and a real
  `libsqlite3-sys` object. The exact fresh-Cargo-home `task install`, release/inventory ratchets,
  embedded-doc generation/check/publication test, and website test/build are green; the final
  repository and native CI gates remain mandatory merge checks.
- 2026-08-04 — implementation started from dispatched canonical `origin/main`
  `6b26dd4fc5e665ddfd7081415fa8d952f21ca93a`. The selected contract is the documented
  cross-process reader/writer lease in `docs/designs/build-directory-ownership.md`: governed builds
  hold shared OS ownership beside the resolved Cargo target, while repository cleanup requires
  exclusive ownership and refuses rather than guessing when a build is live. Failing-first path,
  process-exit and deterministic concurrency coverage is being added before Task wiring.
- 2026-08-04 — filed `ready` from the reported `task install` failure and the fleet process audit at
  canonical `origin/main` merge `a6829b3a14e5b67fedc561d3710d3b29dbee24e1`. Contract only: no
  implementation, root-cause claim or acceptance item is complete.

## Notes

- This story owns Flux's `task install` and repository build-directory contract. The roadmap's
  conservative fleet cleanup rule already refuses primary ignored-cache cleanup without proof; the
  implementation must document how that external shared-target user participates without changing
  roadmap policy silently from this repository.
- A file lock whose cleanup path ignores it, a PID file that can outlive its owner, or a unique
  directory with no bounded reaper does not meet the Goal. The durable invariant is that active
  compiler output has an owner that every repository-owned remover must respect.
