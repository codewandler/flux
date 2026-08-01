---
id: C-209
title: "flux-system tests build fixtures under a transient TMPDIR, reddening the gate at random"
pillar: Core
status: done
priority: 3
epic:
design:
note: "temp_workspace() reads temp_dir() under sandbox::EnvGuard for a documented reason; ~15 other tests call it bare, so a concurrent sandbox test's transient TMPDIR becomes their fixture root and it vanishes underneath them"
---

# flux-system tests build fixtures under a transient `TMPDIR`, reddening the gate at random

## Goal
`cargo test --workspace` fails intermittently in `flux-system`'s lib tests — a **different test each
time**, roughly 1 run in 6 to 1 in 20. It is not a product defect and not any one story's diff, but
it costs an afternoon every time someone meets it mid-gate, because the failure looks like a real
regression in whatever they were working on. Two separate implementors hit it on 2026-07-29 (during
C-207 and C-208) and each spent time proving it was not theirs. Remove the race so a red gate means
something again.

## Goal — the mechanism, already diagnosed
`temp_workspace()` (`crates/flux-system/src/lib.rs:2197-2208`) reads `std::env::temp_dir()` **under
the sandbox env lock**, and says why in a comment:

> Sandbox tests deliberately exercise TMPDIR parsing. Read it under their shared env lock so an
> unrelated process test never builds its fixture under a transient test value.

That is the correct pattern. It is not applied consistently: roughly **15** other call sites in the
same test module call `std::env::temp_dir()` bare — `:2260`, `:2281`, `:2406`, `:2408`, `:2506`,
`:2507`, `:2534`, `:2536`, `:2570`, `:2644`, `:2751`, `:2777`, `:2825`, `:2859`. When a sandbox test
has `TMPDIR` swapped to a transient value at that moment, the bare caller roots its fixture there;
the sandbox test then restores `TMPDIR` and removes that directory, and the victim fails on a path
that has vanished — as a plain assertion or `No such file or directory`, never as anything naming
the real cause.

Observed victims so far (the set is open, which is itself the symptom):
`path_identity_follows_symlink_and_preserves_missing_create_tail` (`:2857`),
`read_root_allows_reads_but_not_writes`, `append_creates_and_appends`.

## Acceptance
- [x] Failing-first: a test that deterministically reproduces the race — e.g. one that mutates
      `TMPDIR` the way the sandbox tests do while another fixture is being constructed bare — and
      fails against the tree as it stands. If a deterministic reproduction proves impractical,
      state why in the story and pin the fix with a structural test instead (see next item), rather
      than declaring the race untestable and moving on.
- [x] Every fixture root in `crates/flux-system/src/lib.rs`'s test module is constructed through one
      guarded helper. No bare `std::env::temp_dir()` remains in the test module.
- [x] A guard prevents regression: a test (or a clippy/codegate rule) fails if a new bare
      `std::env::temp_dir()` appears in that test module. The comment at `:2199-2201` already states
      the invariant — this makes it enforceable rather than advisory.
- [x] `cargo test -p codewandler-flux-system --lib` passes **20 consecutive runs** with no failure.
      Record the command and the result in Progress — a single green run does not close this story.

## Progress
- 2026-07-29 — root cause verified by inspection: the guarded helper at `:2197-2208` versus ~15 bare
  call sites, listed above. Not yet fixed.
- 2026-07-29 — **fixed.** Baseline reproduction on the untouched tree: 25 runs of
  `cargo test -p codewandler-flux-system --lib` → 1 failure,
  `path_identity_follows_symlink_and_preserves_missing_create_tail`, panicking at `lib.rs:2860`
  with `Os { code: 13, kind: PermissionDenied }`. That pins the mutator exactly: `sandbox.rs`'s
  `wrap_argv_rejects_root_from_automatic_tmpdir_too` sets `TMPDIR=/`, so the bare caller rooted its
  fixture at `/flux-sys-path-id-…` and `create_dir_all` was refused. Same race, and on a host where
  `TMPDIR` names a real directory the same window presents as the vanishing path the Goal describes.
- One guarded helper now owns every fixture root in the crate:
  `sandbox::fixture_path`/`fixture_dir` (module scope, beside `EnvGuard`, so **both** test modules
  share it). It reads `std::env::temp_dir()` under `SANDBOX_ENV_LOCK`. A new
  `HOLDS_ENV_LOCK` thread-local plus `env_lock_if_free()` makes the non-reentrant lock safe to ask
  for from inside a test that already holds an `EnvGuard`, which is what let *every* call site use
  one signature instead of a locked/unlocked pair. Zero bare `std::env::temp_dir()` remain in any
  test module; `lib.rs:360` (production `worktree_base_dir`) is untouched.
- **A second leg of the same race was found and fixed.** `SpawnPolicy::for_workspace` reads
  `TMPDIR`/`CARGO_HOME`/`RUSTUP_HOME`/`HOME`, and 12 `sandbox` tests called it with no lock held.
  Measured on the tree with only the fixture-root leg fixed: 1 failure in 40 runs
  (`wrap_argv_creates_configured_writable_dirs_and_uses_a_required_bind`, `sandbox.rs:1784`) and an
  earlier one of `wrap_argv_dispatches_to_seatbelt_when_active` — both `Config("sandbox writable
  root `/` is not allowed …")`, i.e. `TMPDIR=/` observed through production code rather than through
  a fixture root. Those calls now go through the test module's `workspace_policy()` wrapper.
- Regression guard: `tests::no_bare_temp_dir_in_the_test_modules` (`lib.rs`) scans `lib.rs`,
  `sandbox.rs` and `net.rs` via `include_str!`, splits at `mod tests {`, and fails if the test half
  contains a bare `std::env::temp_dir()` or more than the one sanctioned
  `SpawnPolicy::for_workspace` call. Proven to fire: reintroducing a bare `temp_dir()` at the
  `unconfined_lifts_the_sandbox` fixture reddens it, and so does reverting one `workspace_policy`
  call back to `SpawnPolicy::for_workspace` (`left: 2, right: 1`).
- Failing-first evidence for the race test itself: with the `env_lock_if_free()` line deleted from
  `fixture_path` (i.e. the pre-fix bare read), `tests::a_transient_tmpdir_never_captures_a_fixture_root`
  is red **10 out of 10 runs** — `fixture root captured by a transient TMPDIR:
  /tmp/flux-c209-transient-…/flux-c209-victim-…`. Restoring the line makes it green.
- **20-run bar: `cargo test -p codewandler-flux-system --lib` — 60 consecutive runs, 0 failures**
  (20 + 40 in two batches). Full gate green: `cargo build --workspace`, `cargo test --workspace`
  (144 suites ok, 0 failed), `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo fmt --all` (no diff), `cargo test -p flux-codegate` (13 passed).

## Notes
- The obvious fix is to extend `temp_workspace()`'s pattern: one helper that returns a fixture root,
  guarded, and have every test use it. Some call sites want a path *without* a `System`
  (`:2281` `flux-worktree-foreign`, the several `outside` dirs), so the helper likely needs a
  path-only variant rather than a single signature.
- ⚠ Do **not** "fix" this by serialising the whole test module or by adding sleeps. The race is a
  correctness bug in fixture construction; serialising hides it and costs test time forever.
- The `COUNTER`/PID naming scheme is fine and is **not** the problem — names do not collide. The
  problem is the *root* the names hang off, which is why it presents as a vanishing directory rather
  than as a clash.
- Not to be confused with the `/tmp/.git` sticky-test flake (a stray `/tmp/.git` making a flux-flow
  test sticky — an operator-machine note, not a repo artifact; the wiki-style link that used to
  appear here resolved to nothing in this repo, C-332) or with
  ENOSPC-masquerading-as-cargo-errors under disk pressure. Both were separately ruled
  out on 2026-07-29: `/tmp/.git` did not exist, and the failures reproduced with ample free disk.
- Related evidence: C-207's implementor measured 1 failure in 15 runs on its branch and 1 in 20 on
  the merge base — different tests each time, which is what pointed at a shared-environment race
  rather than any single test's logic.
