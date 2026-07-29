---
id: C-92
title: Add hunk-level git_* ops so an agent can stage part of a shared file
pillar: Core
status: in-progress
priority: 6
design: docs/designs/hunk-level-staging.md
note: "whole-file git_stage forces sweeping a coworker's in-flight hunks into an agent commit; the split-file case has no tool"
---

# Add hunk-level git_* ops so an agent can stage part of a shared file

## Goal
Let an agent stage and commit only its own changes when another author is concurrently editing the
same file. Today `git_stage` operates on whole file paths, so a file touched by two authors can only
be staged in full — the agent must either sweep the coworker's uncommitted hunks into its own commit
or hand the task back to the human. This serves Core's worktree-discipline invariant (protect the
user's worktree; make changes auditable) by giving the guarded git surface the granularity `git add
-p` has.

## Acceptance
- [x] A new guarded op stages a subset of a file's changes at hunk granularity (the `git add -p`
      equivalent) — e.g. `git_stage_hunks { path, hunks: [<selector>] }` — routed through the same
      `flux_system` guarded process/IO path as the existing `git_*` ops (no second `Command::new`,
      workspace-pinned, argv-only). Name/shape decided in a short design note first.
- [x] A read op surfaces the addressable hunks of a working-tree file (index vs worktree diff split
      into stable, selectable hunk units) so the staging op has something deterministic to reference.
- [x] The op refuses cleanly (recoverable ToolResult error, not a plan-halting raw error) when a
      requested hunk no longer applies (the file changed underneath), mirroring the fs-tool guidance
      pattern (see C-32).
- [x] Effects/`permission_subjects`/`intents` are accurate — staging is a workspace mutation scoped
      to the named path, so the approval gate sees the right subject (do not return empty subjects).
- [x] A failing-first test proves the split-author case: a file with two independent hunks, stage
      only one, assert the index contains exactly that hunk and the other stays in the working tree.
- [x] Catalog/docs kept in sync: `crates/flux-flow/docs/ops-reference.md` and the `git` group in
      `groups.rs` (+ the `builtins_register` expected-name list).

## Progress
- 2026-07-29 design note landed first (`docs/designs/hunk-level-staging.md`) deciding the selector
  contract, which was the crux: **content-addressed hunk ids, re-verified against a freshly computed
  diff at stage time**. `id = h{ordinal}-{16 hex of SipHash(path \x01 hunk body)}`; the `@@` line
  numbers are deliberately excluded so staging one hunk does not re-key its siblings, and the ordinal
  is a readability/duplicate-disambiguation device only, never the integrity check. Rejected: a bare
  positional index (fails *silently* — a coworker's save between the read and the stage redirects the
  selection to their hunk, which is the exact bug the story exists to prevent) and caller-supplied
  patch bytes (detects drift, but makes the model the author of executable patch text and inverts the
  trust direction; under the id scheme the bytes handed to `git apply` are always ones flux just read
  out of `git diff`).
- 2026-07-29 implemented two ops in `crates/flux-tools/src/lib.rs`, beside the existing `git_*`
  family:
  - `git_hunks {path, context?}` — runs `git --no-pager diff --no-ext-diff --no-color
    --src-prefix=a/ --dst-prefix=b/ --unified=N -- <path>` (config-pinned so the output cannot drift
    with the user's git settings), splits it into `Hunk`s, and lists each as `[id] @@ …  +a -r`
    followed by the hunk verbatim. `Risk::Low` on a new `flux_spec::coherence::EXEMPT` I1 entry —
    same grounds and same scope as `git_diff`'s.
  - `git_stage_hunks {path, hunks, context?}` — **recomputes the diff itself**, rematches the ids,
    reassembles a patch from only the selected hunks (verbatim bytes from that fresh diff), and pipes
    it to `git apply --cached --recount --whitespace=nowarn -`. `Risk::Medium`, `Conditional`.
  - Both: `effects` `[Process]` / `[Process, LocalSystem]`, `access [Process]`, `permission_subjects`
    = the named path via a new `single_path(params, fallback)` helper that falls back to the op name
    rather than ever returning an empty list, one `CommandExecution` intent each.
- 2026-07-29 two independent staleness checks, both recoverable (C-32 pattern, never a raw `Err`):
  an **id miss** returns `stale_hunk_guidance` naming the stale ids, the ids that exist now, and
  `git_hunks(<path>)` to re-run; a **`git apply` rejection** (the staged side moved) surfaces git's
  stderr with the same repair instruction. `git apply` is all-or-nothing, so neither path can leave a
  partial stage.
- 2026-07-29 `flux-system` gained one new guarded run variant, `System::run_with_stdin` — `git apply
  --cached -` takes its patch on stdin and every existing `run*` helper nulls it. It routes through
  the same `build_command` choke point and the same `await_process` capture as `run_with_env` (no
  second `Command::new`); the payload is written from its own task so a patch larger than the pipe
  buffer cannot deadlock against the output capture.
- 2026-07-29 failing-first tests (all in `crates/flux-tools/src/lib.rs`, rooted through the existing
  `git_ctx()` fixture): `staging_one_hunk_leaves_the_other_authors_hunk_in_the_working_tree` (the
  split-author case — 20-line committed baseline, edits at lines 2 and 18, stage only ours, assert
  the index holds `+line 2 OURS` and not `THEIRS` while the working tree still holds `+line 18
  THEIRS`), `stage_hunks_refuses_cleanly_when_the_file_changed_underneath`, and
  `the_hunk_ops_declare_coherent_path_scoped_metadata` (asserts `flux_spec::metadata_violations` —
  which since C-210 also reads `semantic_effects` — is empty and the subject is the named path).
  All three failed at the merge base with `error[E0425]: cannot find value \`GitHunksTool\` in this
  scope` / `\`GitStageHunksTool\``. Plus `run_with_stdin_feeds_a_payload_larger_than_the_pipe_buffer`
  and `run_with_stdin_closes_the_pipe_so_the_child_sees_eof` in `flux-system`.
- 2026-07-29 catalog/docs synced: `groups.rs` `git` group, the `builtins_register` expected-name
  list, `try_register_builtins`, and `crates/flux-flow/docs/ops-reference.md`.
- 2026-07-29 `website/docs/language/ops.md` also needed the two rows: `flux-cli`'s
  `operations_reference_covers_the_registered_public_catalog` contract test fails the gate if a
  registered public op is missing from the website reference. Caught by the gate, not by the
  Acceptance list, which names only the `flux-flow` reference — worth folding into the last
  Acceptance bullet's wording for future catalog stories.

## Notes
- Motivating incident: an agent implementing `/effort` had to commit into a tree a coworker was
  editing; `engine.rs` and a design doc were shared files, and whole-file `git_stage` would have
  pulled the coworker's D-175/D-178 hunks into the agent's commit. Whole-file staging cannot separate
  two authors editing one file; only hunk-level staging can.
- Relevant surface: the existing `git_*` ops in `flux-tools` (spec + `permission_subjects` +
  `intents` + `execute`, IO via `ctx.system`); the `git` tool group in `groups.rs`. All OS process
  creation must go through `flux_system::System` (`build_command`) — argv-only, no shell string.
- Design-first: the hunk-selector contract (index-based? context-hash? unified-diff patch bytes?) is
  the crux and belongs in `docs/designs/` before implementation.
- Non-goal: interactive `git add -p` prompting — the op takes an explicit hunk selection, since the
  guarded envelope has no interactive TTY.
