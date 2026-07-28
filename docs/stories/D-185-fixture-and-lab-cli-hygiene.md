---
id: D-185
title: Fixture and Lab-CLI hygiene — record/test name validation, atomic session copy, diff rendering
pillar: Agent
status: done
epic: deterministic-agent-lab
design: docs/designs/deterministic-agent-lab.md
priority: 4
note: "review finding (2026-07-28): minor hygiene batch across flux record/test, copy_session_to, and the run-diff fallback renderer"
---

# Fixture and Lab-CLI hygiene — record/test name validation, atomic session copy, diff rendering

## Goal
Batch of confirmed minor defects from the 2026-07-28 epic review, none release-blocking, all worth
closing before the Lab sees external use:

1. **`flux record` accepts `.` and `..` as scenario names** (`crates/flux-cli/src/lab_cmd.rs` —
   the check rejects separators but not dot segments): `flux record .. "x"` targets
   `tests/scenarios/..` = `tests/`, and because no `scenario.toml` exists there the clobber guard
   never trips — fixture files land outside the scenarios root.
2. **`flux test <name>` has no name validation at all** (`discover_fixtures` does `dir.join(name)`),
   so `flux test ../../anywhere/fixture` replays an arbitrary path outside `--dir`. Asymmetric with
   `run_record`'s guard.
3. **`EventStore::copy_session_to` is not atomic** (one transaction per event): a mid-loop failure
   leaves a partially-copied destination session already minted in the registry; a retry creates a
   second half-session beside the orphan. Fresh ULIDs mean no corruption — the defect is stranded
   half-sessions listing as real ones.
4. **Fixture timestamp inversion**: the destination's `SessionStarted` is stamped `now_ms` while
   copied events keep their original older `ts_ms`, so fixture sessions show
   `created_at > updated_at`. Cosmetic.
5. **Fallback diff rows render as raw keys**: `cell_rows` synthesizes `stmt = "call:{op}:{hash16}"`,
   which never resolves through `stmt_texts`, so `render_run_diff` prints `<call:op:abcd…>` instead
   of something readable for natively-dispatched runs.

## Acceptance
- [x] `flux record` and `flux test` both reject any name that is not a single plain path segment
      (no separators, not `.`/`..`); tests pin both commands.
- [x] `copy_session_to` copies atomically (single transaction per backend, or
      write-then-register-last so a partial copy is never listed); test pins that a failed copy
      leaves no listed session.
- [x] Fixture sessions carry a `SessionStarted`/registry timestamp consistent with the copied
      events (no `created_at > updated_at`).
- [x] `render_run_diff` prints a readable label for synthesized cell rows (e.g. the op name and
      hash prefix) instead of an unresolvable `<call:…>` placeholder.
- [x] `--store` with a relative path: either resolve to an absolute path before exporting
      `FLUX_STORE_DIR`, or document that subprocesses with a different cwd resolve it elsewhere.

## Progress
- 2026-07-28: Implemented all five items.
  1. `crates/flux-cli/src/lab_cmd.rs` — hoisted a shared `validate_fixture_name` (empty / `/` /
     `MAIN_SEPARATOR` / `.` / `..` all rejected) used by both `run_record` and `discover_fixtures`
     (`flux test <name>`).
  2. `crates/flux-events/src/store/mod.rs` — replaced `copy_session_to`'s per-event
     `append_at` loop with a new `EventBackend::copy_session_atomic(info, events)` primitive
     implemented per backend (`sqlite.rs`, `postgres.rs`): mints the `streams` row, the
     destination `SessionStarted`, and every copied event inside ONE transaction, so a mid-copy
     failure (e.g. the existing unmappable-`turn_id` case) rolls back cleanly and leaves nothing
     listed. The now-dead `EventBackend::append_at` (both impls) was removed rather than left
     unused.
  3. Same primitive fixes the timestamp inversion for free: `streams.created_at` is stamped from
     the SOURCE session's own `created_at_ms` (never `now_ms()`), `updated_at` from the last
     copied event's `ts_ms` (or the source's `updated_at_ms` when there are no non-`SessionStarted`
     events to copy) — so `created_at <= updated_at` always holds for a copied/fixture session.
  4. `crates/flux-events/src/projection.rs` — `render_run_diff`'s `text` closure now special-cases
     a `"call:{op}:{hash16}"` synthesized key (from `cell_rows`, the natively-dispatched fallback)
     and renders `` op `{op}` ({hash16}…) `` instead of falling through to the unresolvable
     `<call:…>` placeholder.
  5. `crates/flux-cli/src/dispatch.rs` — a relative `--store <DIR>` is now joined onto the
     process's own cwd (computed before the export, not canonicalized — the target directory may
     not exist yet) before being exported as `FLUX_STORE_DIR`; an absolute path passes through
     unchanged, and no `--store` flag leaves any existing `FLUX_STORE_DIR` untouched.

  Tests added: `crates/flux-cli/tests/agent_lab.rs`
  (`flux_record_rejects_names_that_are_not_a_single_plain_segment`,
  `flux_test_rejects_names_that_are_not_a_single_plain_segment`); `crates/flux-events/src/store/mod.rs`
  (`copy_session_to_errors_loudly_on_an_unmappable_turn_id` extended to also assert `dst.list(10)`
  is empty after the failed copy; new
  `copy_session_to_keeps_registry_timestamps_consistent_with_copied_events`);
  `crates/flux-events/src/projection.rs`
  (`render_run_diff_renders_a_readable_label_for_synthesized_cell_rows`).

  Gate: `cargo test -p codewandler-flux-events` (66 unit + 1 doctest, incl. `--features postgres`
  compile check — no live Postgres available to run its tests against), `cargo test -p flux-cli`
  (all suites incl. `agent_lab`), `cargo test -p codewandler-flux-sdk --features test-kit --test
  agent_golden` — all green. `cargo clippy -p codewandler-flux-events -p flux-cli --all-targets -D
  warnings` (plain and `--features postgres`) clean. `cargo fmt -p codewandler-flux-events -p
  flux-cli -- --check` clean.

## Notes
- The `.gitignore` sidecar gap from the same review (`*.db-wal`/`*.db-shm` not un-ignored under
  `tests/scenarios/`) was fixed directly on 2026-07-28 alongside the review; this story is the
  remainder.
