# flux — dogfood notes

Friction captured by driving flux's own agentic mode (`flux run`) on real coding work. The point
is not whether flux produces a perfect patch — it is **where using flux as a coding agent is painful**.
This list, not an audit, sets the real priority order (see [vision.md](vision.md)).

Protocol: `flux run --yes -m <model> -p "<task>"` against an **isolated scratch workspace** (never
this repo); transcripts captured separately. Severity: **P0** blocks the task · **P1** forces a
workaround / retry loop · **P2** friction/polish.

## Batch 1 — 2026-06-25 (flux 0.2.2, model anthropic/opus)

flux completed **all 5 tasks correctly** — it is a capable coding agent (clean greenfield scaffolding,
an accurate line-cited trace of a large unfamiliar codebase, a correct bugfix, a feature with tests, a
multi-site rename with no edit retries). **Every friction point was in the tooling/UX layer, not
capability.**

| # | Task | Workspace | Completed? | Friction (severity) |
|---|------|-----------|------------|---------------------|
| 1 | greenfield: a `stats` CLI from scratch | greenfield | ✅ (10 tests pass) | raw markdown output (P2); tool-output truncated in display (P2) |
| 2 | navigate/explain the safety-envelope dispatch | flux-copy | ✅ (accurate, line-cited) | **`grep`/`glob` with a file `path` → "no matches" (P1)**; raw markdown (P2) |
| 3 | bugfix: make the failing test pass | sample | ✅ (single edit, no retry) | had to re-run `cargo test` 3× with head/tail/grep to see results past the cap (P2) |
| 4 | feature: add `title_case` + test + wire to CLI | sample | ✅ (8 tests pass) | — |
| 5 | rename `slugify`→`to_slug` across the crate | sample | ✅ (0 left, 5 edits, no retry) | — |

### Friction detail

**F1 (P1) — `grep`/`glob` with a file `path` silently returns "no matches".** In Task 2, flux ran ~7
greps scoped to a specific file (e.g. `grep {path:"crates/flux-runtime/src/lib.rs", pattern:"struct
Executor"}`) that all returned **"no matches"**, then the *same* pattern grepped unscoped found it at
`lib.rs:325`. Root cause: `System::walk_files` (`crates/flux-system/src/lib.rs:236`) only ever
`read_dir`s the resolved `base`; when `base` is a file, `read_dir` errors → the walk yields nothing →
`GrepTool`/`GlobTool` (`crates/flux-tools/src/lib.rs:685,621`) report "no matches". The tool doc calls
`path` a "subdirectory", but scoping a search to one file is the natural thing to ask. Wastes turns and
can mislead the agent into concluding a symbol doesn't exist. **→ fix in `walk_files` (return the file
itself when `base` is a file); regression test in flux-tools.**

**F2 (P2) — assistant output is raw, unrendered markdown.** Every task's final answer used `##`
headers, `**bold**`, and `` `code` ``, printed literally to the terminal. Readable but not pleasant;
the #1 readability item deferred at 0.2.0. **→ tracked as an enhancement (bigger change: a renderer in
`flux-cli`'s `CliSink`); file an issue.**

**F3 (P2) — tool output is truncated in the CLI display with no way to see it.** The CLI streams a
one-line preview of each tool result and truncates with `…` (e.g. Task 1's `cargo test` output was cut
to `…in 0.14s…`, hiding the "10 passed" line; in Task 3 flux re-ran `cargo test` three times piping
through `head`/`tail`/`grep` to fit results into the cap). The user can't see what the agent saw. **→
fix: show a larger multi-line tool-result preview in `CliSink`; unit-testable. (Affects display only —
the agent receives the full, separately-capped result.)**

## Outcome (shipped in 0.2.3)

**Fixed this iteration — each with a regression test that fails first + an end-to-end re-run:**

1. **F1 — `grep`/`glob` file-path (P1, correctness).** Fixed in `System::walk_files`
   (`crates/flux-system/src/lib.rs`): when `base` resolves to a file, return that file instead of an
   empty walk. Regression test `grep_searches_a_single_file_path` (`crates/flux-tools`). **Re-run
   confirmation:** re-running Task 2 on the fixed binary produced **0** file-scoped "no matches" (was
   ~7) and finished in **52s vs 81s** — the wasted retry greps are gone.
2. **F3 — tool-output preview (P2).** Fixed with a multi-line `tool_preview` in `flux-cli`
   (`crates/flux-cli/src/main.rs`): up to 12 lines, continuation lines indented, with a
   `… (+N more lines)` note; replaces the one-line 200-char truncation. Tests in flux-cli (the crate's
   first unit tests). **Re-run confirmation:** `read`/`glob`/`grep` results now render across lines
   (e.g. `✓ read: …` followed by 12 indented lines + `… (+108 more lines)`).

**Tracked (open) — filed as a GitHub enhancement issue:**

3. **F2 — markdown rendering of assistant output (P2).** The final answer prints raw `##`/`**`/`` ` ``;
   a renderer in `CliSink` is a larger change. Deferred to a follow-up.
