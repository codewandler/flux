---
id: D-66
title: Schema-SSoT increment — handler-parsed structs for the simple 1:1 flux-tools ops
pillar: Core
status: done
design:
epic:
note: the drift-ledger-recorded follow-up to D-34/D-36 (docs/archive/drift-reports.md) — ~330 schema-only #[allow(dead_code)] structs; only `write` (flux-tools) and `task` (flux-orchestrate) are full SSoT today; tranche-sized by design
---

# Schema-SSoT increment — handler-parsed structs for the simple 1:1 flux-tools ops

## Goal
The D-34/D-36 migrations made every op schema derive from a typed struct, but most handlers still
do ad-hoc `&Value` extraction beside an `#[allow(dead_code)]` schema-only struct — the recorded
follow-up ("full SSoT where the handler is a simple 1:1 field extraction") is unstarted beyond
`write` and `task`. Convert the first tranche: the flux-tools ops whose handlers are plain field
extraction, using the proven `parse_params` pattern, so schema and parsing cannot drift and the
allows retire with it.

## Acceptance
- [x] Tranche scoped in Progress before converting: list the flux-tools ops that qualify as
      simple 1:1 extraction (exclude the flux-eval `coerce_json` convention ops — their JSON-string
      coercion is documented as incompatible with blanket struct deserialize).
- [x] Each converted handler parses its schema struct via `parse_params`; the struct's
      `#[allow(dead_code)]` is removed; behavior pinned by the existing op tests (plus a
      failing-first test anywhere semantics could shift — e.g. type-mismatch error paths).
- [x] No schema contract change: derived schemas byte-identical (or the no_manual_schema /
      contract tests updated with the diff explained in Progress).
- [x] The drift ledger (`docs/archive/drift-reports.md`, relocated by C-42) updated: converted ops move out of the
      "schema-only by design" bucket.

## Progress

**Tranche scoped** (before converting): `flux-tools` ops whose handler is plain field extraction
matching its already-derived schema struct exactly, with zero numeric fields (see "excluded"
below for why numeric fields disqualify an op this round).

Converted (12 in `crates/flux-tools/src/lib.rs` + 2 in `crates/flux-tools/src/evidence.rs`, plus
one cleanup):
- `edit`, `glob`, `append`, `read_many`, `git_stage`, `git_commit`, `git_status`, `git_diff`,
  `git_push`, `git_checkout`, `git_unstage`, `flux_reload`
- `evidence`, `metrics`
- `observe` — found *already* parsing via `parse_params` (an earlier, uncommitted pass), just
  still carrying a stale `#[allow(dead_code)]`; removed as a no-behavior-change cleanup.

Excluded this round (see `docs/archive/drift-reports.md`'s new D-66 section for full reasoning):
- `read`, `grep`, `bash`, `proc.run`, `git_log` — all read a numeric field via the `u64_arg`
  helper, which deliberately tolerates a JSON string (`"120"`) that a strict `u64` struct field
  would reject; a real regression for models that emit stringly-typed numbers (there's an
  existing test, `read_coerces_string_offset_limit`, guarding exactly this for `read`).
- `patch` — numeric fields (same reason) plus non-trivial custom validation logic, not 1:1
  extraction.
- `cargo_check`/`cargo_build`/`cargo_test`/`cargo_clippy`/`cargo_fmt` — no numeric fields and no
  aliasing, but their argv-building helpers take `&Value` directly (and are unit-tested with raw
  `Value` today); converting cleanly means threading typed struct fields through those shared
  helpers and rewriting their existing tests too — a bigger, riskier diff deferred to tranche 2.
- `toolchains.rs`, `extra.rs`, `cognition.rs` — not audited this pass; left for a future tranche
  so this pass stayed fully reviewed rather than rushed.
- `reflect.rs` (`plan`/`run_plan`/`op.register`) — stays excluded per the existing D-31 ledger
  entry (validate-only forwarding / richer-schema-than-runtime-type).

**Drift found** (documented in the ledger, not silently absorbed): both converted-struct families
already carry `#[serde(deny_unknown_fields)]` (the D-31/D-34 convention, `write` included), so
converting the handler to `parse_params` newly *enforces* two things the schema already
published but the ad-hoc handler didn't: (1) unknown/extra keys now hard-error instead of being
silently ignored, and (2) a non-string element in a `paths: Vec<String>` array now hard-errors
instead of being silently dropped by the old `filter_map`-based extraction. Both are treated as
aligning behavior with the already-published schema (the same call the `write` conversion already
made in D-31, without a separate callout) rather than new drift. Pinned with two failing-first
tests: `append_rejects_unknown_field` and `read_many_rejects_non_string_path_element` (both
verified to fail against the pre-conversion handler, for the right reason, before being kept as
permanent regression/contract tests).

**New test coverage**: the git ops (`git_stage`/`git_commit`/`git_status`/`git_diff`/`git_push`/
`git_checkout`/`git_unstage`) had zero direct execute-level tests before this tranche. Added
`git_ops_stage_commit_status_diff_unstage_checkout` (local-repo end-to-end: stage → status →
commit → modify → unstaged diff → stage → staged diff → unstage → checkout -b) and
`git_push_pushes_to_a_local_remote` (push to a local bare repo, no network needed). `edit`/
`append`/`glob`/`read_many` already had passing tests exercising `.execute()`; those continued to
pass unmodified through the conversion (the regression pin for "everything else stays the same").
`evidence`/`metrics`/`observe` were already exercised via `Executor::dispatch` in `evidence.rs`'s
own tests and continued to pass unmodified.

**Left ad-hoc on purpose** (to keep the diff minimal/low-risk): `permission_subjects`/`intents`
for the converted tools still do their own lightweight `&Value` lookups rather than parsing the
full struct — only `execute()` (the Acceptance's "handler") was converted. This mirrors the
existing `edit`/`append`/etc. code, just not `write`'s (which also typed its `permission_subjects`/
`intents`). Not required by Acceptance; noted here rather than silently expanded.

**Gate**: `cargo test -p flux-tools` (82 tests total — 81 unit + 1 integration — all green,
including the two failing-first tests
run against both the old and new handler bodies), `cargo clippy -p flux-tools --all-targets -- -D
warnings` (clean), `cargo fmt -p flux-tools` (clean). Full-workspace gate was not re-run (other
agents are concurrently working elsewhere in the tree per the task's constraints); this crate's
public API surface (the `Tool` trait impls) is unchanged, only private struct/handler internals,
so no downstream crate should be affected.

## Notes
- Repeatable story shape: later tranches (remaining flux-tools ops, then plugins via host-kit)
  can clone this story. Plugin-side conversion is a separate, larger decision — the D-34
  precedent deliberately kept plugin handlers schema-only.
- Tranche 2 candidates, roughly in order of expected payoff: (a) a `u64_arg`-preserving
  `deserialize_with` shim (keeps the Rust field type — and therefore the derived schema —
  unchanged) to unlock `read`/`grep`/`bash`/`proc.run`/`git_log`; (b) the `cargo_*` argv-builder
  refactor; (c) an audit pass over `toolchains.rs`/`extra.rs`/`cognition.rs` for further simple
  1:1 candidates.
