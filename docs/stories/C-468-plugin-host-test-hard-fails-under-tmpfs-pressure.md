---
id: C-468
title: "A plugin-host test copies a binary into /tmp and hard-fails when the tmpfs is full"
pillar: Core
status: done
priority: 8
areas: [flux-plugin]
note: "spawn_refuses_hash_drift now stages its writable binary beside Cargo's active target fixture; copy failures name the scratch path and storage cause before hash verification"
---

# A test that blames the code for a full disk

## Goal

Make `spawn_refuses_hash_drift` fail *legibly* — or skip — when `/tmp` cannot hold the binary it needs
to copy, so a full tmpfs stops looking like a plugin hash-verification regression.

## The finding

`crates/flux-plugin/tests/host.rs:229-231`:

```rust
let dir = std::env::temp_dir().join(format!("flux-spawn-drift-{}", std::process::id()));
std::fs::create_dir_all(&dir).unwrap();
let stored = dir.join("flux-plugin-echo");
std::fs::copy(exe, &stored).unwrap();          // ← a whole test binary, into /tmp
```

The test needs a *writable copy* of a plugin binary so it can append `b"tampered"` to it (`:250-256`)
and prove that hash drift refuses to spawn. That design is right. Its choice of location is not:
`std::env::temp_dir()` is `/tmp`, which on this repo's development machines is a **RAM-backed tmpfs**,
separate from `/` and much smaller — and flux itself documents this hazard at
`crates/flux-system/src/lib.rs:470` (*"`/tmp` is commonly a RAM-backed tmpfs, and a build inside an
entered … "*) and again at `:2879`.

When that tmpfs is full, the `.unwrap()` panics with `QuotaExceeded`. The test name in the failure
output is `spawn_refuses_hash_drift`, so the report reads as *plugin hash verification broke* — the
guarded-IO area where a false alarm costs the most investigation. It is a full disk.

⚠ This has bitten a real gate run. It is not hypothetical.

## Acceptance

- [x] A failing-first test, or an equivalent demonstration, that the copy's failure is reported as an
      environment problem: the message names the temp directory and the ENOSPC/quota cause, and does
      **not** read as a hash-verification failure.
- [x] The test either uses a target-adjacent scratch directory instead of `/tmp` (flux already prefers
      an on-disk location over the tmpfs elsewhere — reuse that reasoning, and `FLUX_WORKTREE_DIR`'s
      precedent at `flux-system/src/lib.rs:2879`), or skips with a clear message when the copy cannot be
      made.
- [x] ⚠ Skipping must be **loud**. A silently-skipped test that verifies hash drift is worse than a
      confusing failure: the guard would be unverified and the run still green. If it skips, it says so
      in the output.
- [x] The tamper-and-refuse assertions at `:262-265` (names the plugin, the expected hash, the actual
      hash) are unchanged — this story changes where the fixture lives, never what it proves.
- [x] ⚠ Grep the tree for sibling tests with the same `temp_dir()` + copy-a-binary shape and fix them
      together, or say which were left and why.

## Progress

- 2026-08-03: added a failing-first test against an injected `ENOSPC / quota exceeded` copy error; it
  initially failed to compile because the contextual staging helper did not exist.
- 2026-08-03: the helper now derives a unique scratch directory from
  `CARGO_BIN_EXE_echo_plugin`'s parent, so custom target directories and profiles stay on the same
  filesystem as the already-built fixture. An RAII guard removes the exact directory on every exit.
- 2026-08-03: fixture staging remains a loud failure rather than a skip. Its message names the
  target-adjacent directory, source, destination, underlying IO cause, and that verification had not
  started. The original plugin/expected-hash/actual-hash assertions remain intact.
- 2026-08-03: a tree sweep found no sibling `temp_dir()` plus `CARGO_BIN_EXE_*` binary-copy test.
  Other `fs::copy` sites move production pack artifacts or small test fixtures and were left alone.
- 2026-08-03: both focused tests and all nine `flux-plugin` host integration tests pass.
- 2026-08-03: the full repository gate passes: workspace build and tests, clippy with warnings denied,
  formatting, and `flux-codegate`.

## Notes

- Verified 2026-08-02 against `main`.
- Related to the repo's standing tmpfs hazard: a full `/tmp` also makes unrelated commands exit with
  empty output, so this test's panic often arrives alongside other confusing symptoms — one more reason
  its message should name the cause.
- Filed 2026-08-02 while integrating the D-232 wave.
- The test deliberately does not skip: Rust's harness captures output from passing tests, so a
  printed skip would not be loud in the ordinary gate.
