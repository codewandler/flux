# Flux-Flow — Registered ops

The operations the engine advertises to the planner. These are an **engine** concern (provided by
`flux-tools` and surfaced through the live `ToolRegistry`), not part of the Flux-Lang language — see
[`flux-lang/docs/reference.md`](../../flux-lang/docs/reference.md) for the language itself.

## Registered ops quick reference

Ops are passed by name to `call`. Arguments are positional in the order shown;
optional arguments are in `[brackets]`.

| op | signature | risk | description |
|---|---|---|---|
| `read` | `path[, limit, offset]` | Low | Read one file (string path), a list of files (JSON array), or a glob pattern (string with `*`/`?`). Single-file: line-numbered view, paging via `offset`/`limit`. Multi-file/glob: sections headed `==> path <==`. Guidance returned for over-cap files. |
| `grep` | `pattern[, glob, literal, max_results, path]` | Low | Search by regex (supports `\b`, lookaheads); use `literal: true` for plain substring |
| `glob` | `pattern[, path]` | Low | List files matching a glob pattern (`*` crosses `/`) |
| `search` | `query[, limit]` | Low | Search the indexed datasource |
| `web_fetch` | `url` | Low | Fetch an HTTP(S) URL |
| `write` | `path, content` | Medium | Write (create/overwrite) a file |
| `edit` | `path, old_string, new_string[, replace_all]` | Medium | Replace a string in a file (must match exactly once unless `replace_all`); if the exact text isn't found, progressively looser matching is tried (trailing whitespace → indentation drift → first/last-line anchor) and the result reports which strategy matched |
| `patch` | `path, edits` | Medium | Apply several line-anchored edits in one call; each edit is `{op, line, end_line?, text?}` where op is `insert_before`, `insert_after`, `replace_range`, or `delete_range`; ALL line numbers refer to the original file |
| `append` | `path, content` | Low | Append to a file (creates it and parent dirs if absent); lower-risk than `write` |
| `read_many` | `paths` | Low | Read several files at once (each section headed `==> path <==`); prefer single `read` when you need to embed a file's text into a later string |
| `task` | `role, task` | Medium | Delegate to a sub-agent role |
| `bash` | `command[, timeout_secs]` | High | Run a shell command |
| `file_stat` | `path` | Low | File metadata: size, line count, mtime (replaces `wc -l`, `stat`, `ls -la`) |
| `path_exists` | `path` | Low | Returns `"true"`/`"false"` — use with `when`/`unless` to branch on file presence |
| `sqlite_query` | `db, sql[, params]` | Low | Read-only SQLite query (SELECT/PRAGMA only) |
| `web_search` | `query[, max_results]` | Low | Tavily web search — requires `TAVILY_API_KEY` env var |
| `cargo_check` | `[package, args]` | Medium | `cargo check` (type-check only, no codegen) |
| `cargo_build` | `[package, release, args]` | Medium | `cargo build` |
| `cargo_test` | `[package, filter, args]` | Medium | `cargo test` |
| `cargo_clippy` | `[package, args]` | Medium | `cargo clippy` |
| `cargo_fmt` | `[package, check]` | Medium | `cargo fmt` (pass `check: true` to only verify) |
| `git_stage` | `paths` | Medium | Stage files (`git add`) |
| `git_commit` | `message[, body]` | Medium | Create a commit |
| `git_status` | | Low | Working tree status |
| `git_diff` | `[path, staged]` | Low | Show unstaged (or staged) diff |
| `git_log` | `[limit]` | Low | Recent commits |
| `git_push` | `[branch, remote]` | Medium | Push to remote |
| `git_checkout` | `branch[, create]` | Medium | Switch/create branch |
| `git_unstage` | `paths` | Low | Unstage files |

`write`, `edit`, `patch`, `append`, `task`, `bash`, and the `cargo_*` ops may pause for user approval
(controlled by the safety envelope and the active permission rules).
