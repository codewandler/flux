---
id: C-192
title: "sqlite_query reaches arbitrary-path file creation outside guarded IO via VACUUM INTO"
pillar: Core
status: ready
priority: 2
epic: security-assurance
design: docs/designs/security-assurance.md
note: "REVIEW — the first CONFIRMED bypass of the guarded-IO invariant: a default-registered Risk::Low/Effect::Read op authorized as a workspace read can create a file at any absolute path the process can write, never touching flux-system"
---

# sqlite_query reaches arbitrary-path file creation outside guarded IO via VACUUM INTO

## Goal
`docs/architecture.md:157` says guarded IO is "the **only** place real filesystem / process / network
IO happens" and `AGENTS.md:16` says "there are no bypass paths." `sqlite_query` is one. Close it, and
close it in a way that makes the *class* harder to reintroduce rather than patching the single
keyword that was missed.

## Acceptance
- [ ] Failing-first test: dispatching `sqlite_query` with a workspace-relative `db` and
      `sql: "VACUUM INTO '<absolute path outside the workspace>'"` must not create that file. This
      test must fail against the current tree — that failure is the proof the gap is real.
- [ ] The `sql` input is admitted by a **statement-type allowlist**, not the `is_write_sql` keyword
      denylist (see C-193, which should land in the same change).
- [ ] Regression test: the same call with the target *inside* the workspace is either refused for the
      same reason or routed through `flux-system` — a workspace-internal write from an op declaring
      `Effect::Read` is still a misdeclaration, not an acceptable outcome.
- [ ] Whichever disposition is chosen, the tool's `ToolSpec` matches it. If any write remains
      reachable, `effects` gains `Effect::Write` so the derivation at
      `flux-runtime/src/lib.rs:2328` emits `workspace_write` and the `unscoped_write` approval
      trigger (`:3488`) can see it.
- [ ] `docs/architecture.md:169`'s no-direct-IO invariant is either honoured by this tool (DB opened
      through `flux-system`) or the deviation is named explicitly in the code with the guard that
      replaces it — no silent hand-rolled jail.

## Progress
- (not started)

## Notes
- **Verified against the tree at `0.33.1` (f8e90d7).** Source review:
  [`reviews/2026-07-29-envelope-integrity.md`](../../reviews/2026-07-29-envelope-integrity.md),
  finding 1.
- The declaration: `crates/flux-tools/src/extra.rs:278-282` —
  `effects: vec![Effect::Read, Effect::Filesystem]`, `risk: Risk::Low`,
  `access: vec![AccessKind::Filesystem]`. No `Effect::Write`.
- What the policy therefore authorizes: `crates/flux-runtime/src/lib.rs:2335-2341` derives
  **`workspace_read(<db path>)`** and nothing else. The `unscoped_write` approval trigger at `:3488`
  tests `spec.effects.contains(&Effect::Write)` and is false, so no approval gate fires either.
- Where guarded IO is skipped: `extra.rs:341-348` opens the database with `rusqlite` directly;
  `:350-360` runs the model-supplied `sql` via `prepare` + `query`. `SQLITE_OPEN_READ_ONLY` is the
  only guard. The hand-rolled `jail_sqlite_path` (`:237`) constrains the **`db`** path only — the
  `VACUUM INTO` target never passes through it, nor through `flux-system`'s confinement, symlink
  rejection or canonicalization.
- Why the read-only flag does not cover it: `VACUUM INTO` is read-only with respect to the *source*
  database. `VACUUM` is absent from the `is_write_sql` denylist (`:209-212`).
- **SQLite semantics pinned empirically** in an isolated scratch database (flux was not run, nothing
  was exploited): under autocommit on a `mode=ro` connection — `INSERT` blocked by the flag;
  `VACUUM INTO '<fresh absolute path>'` succeeds, 8192 bytes; a second `VACUUM INTO` to the same path
  refused, *"output file already exists"*.
- **Bounding the primitive honestly:** file *creation* only — the target must not already exist, so
  this is not arbitrary file modification. Content control is partial: table names and row values
  land in the page bytes as plain text, the rest is SQLite structure.
- Reachability: registered by `try_register_extra` (`extra.rs:597-609`), called unconditionally from
  `try_register_builtins` (`crates/flux-tools/src/lib.rs:231`); the default-catalog test at
  `lib.rs:4200` asserts its presence. It is **not** behind a group signal, unlike `bash`
  (`lib.rs:4213`).
- **Operator mitigation until this lands:** `[tools] disable = ["sqlite_query"]`. That check runs
  first and unconditionally in `gate` (`flux-runtime/src/lib.rs:3272`), ahead of scope, hooks, policy
  and permission rules.
- Relationship to [C-191](C-191-toolspec-invariant-test.md): C-191 asserts a `ToolSpec` is
  *internally coherent*. This tool's spec **is** coherent — it is merely unfaithful to what `execute`
  does, which C-191 cannot catch. See [C-194](C-194-no-direct-io-lint.md) for the fidelity half.
