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
| `now` | | Low | Current wall-clock time: unix seconds + UTC string (replaces `date`) |
| `cwd` | | Low | Absolute path of the workspace root (replaces `pwd`) |
| `sys_info` | | Low | Host metadata: os, arch, family, hostname (replaces `uname`) |
| `cargo_check` | `[package, args]` | Medium | `cargo check` (type-check only, no codegen) |
| `cargo_build` | `[package, release, args]` | Medium | `cargo build` |
| `cargo_test` | `[package, filter, args]` | Medium | `cargo test` |
| `cargo_clippy` | `[package, args]` | Medium | `cargo clippy` |
| `cargo_fmt` | `[package, check]` | Medium | `cargo fmt` (pass `check: true` to only verify) |
| `python_run` | `[script, module, args]` | Medium | Run a Python script or `-m module` (python group) |
| `pytest` | `[path, args]` | Medium | Run `pytest` (python group) |
| `npm` | `args` | Medium | Run an `npm` command, e.g. `["run","build"]` (node group) |
| `node_run` | `script[, args]` | Medium | Run a JavaScript file with `node` (node group) |
| `go_build` | `[package, args]` | Medium | `go build` (default `./...`; go group) |
| `go_test` | `[package, args]` | Medium | `go test` (default `./...`; go group) |
| `go_vet` | `[package, args]` | Medium | `go vet` (default `./...`; go group) |
| `make` | `[target, args]` | Medium | Run `make` (make group; surfaces on a `Makefile`) |
| `git_stage` | `paths` | Medium | Stage files (`git add`) |
| `git_commit` | `message[, body]` | Medium | Create a commit |
| `git_status` | | Low | Working tree status |
| `git_diff` | `[path, staged]` | Low | Show unstaged (or staged) diff |
| `git_log` | `[limit]` | Low | Recent commits |
| `git_push` | `[branch, remote]` | Medium | Push to remote |
| `git_checkout` | `branch[, create]` | Medium | Switch/create branch |
| `git_unstage` | `paths` | Low | Unstage files |

`write`, `edit`, `patch`, `append`, `task`, `bash`, and the toolchain ops (`cargo_*`, `go_*`,
`python_run`, `pytest`, `npm`, `node_run`, `make`) may pause for user approval (controlled by the
safety envelope and the active permission rules).

## Cognition ops

The cognition pack (group `cognition`). **Pure** ops are deterministic, no-IO data shaping (always
advertised). **Model-backed** ops do one structured model call — they live in the `flux-cognition`
crate and are only advertised once a host registers `CognitionPack::new(provider, model)` into the
registry.

| op | kind | signature | description |
|---|---|---|---|
| `need` | pure | `ask, require[, done_when]` | Build a `Need` artifact (an explicit statement of missing info) |
| `gaps` | pure | `claims, need` | Report a `Need`'s still-unmet `require` fields given some claims |
| `compare` | pure | `a, b` | `{ added, removed, common }` over two arrays |
| `dedupe` | pure | `items[, by]` | Remove duplicates (whole-value, or by a field), first-seen order |
| `sort` | pure | `items[, by, order]` | Stable sort by a field (or natural); `order` = `asc`/`desc` |
| `top` | pure | `items, n` | The first `n` items |
| `merge` | pure | `lists` | Concatenate an array-of-arrays into one array |
| `cite` | pure | `claims` | A markdown citation list, one line per claim |
| `len` | pure | `items` | Count of an array's items (or a string's characters) |
| `first` | pure | `items` | The first item of an array (or `null`) |
| `last` | pure | `items` | The last item of an array (or `null`) |
| `filter` | pure | `items[, by, equals]` | Keep items where a field/value is truthy (or equals a value) |
| `ai.extract` | model | `from[, ask, schema]` | Extract typed items (e.g. `Claim[]`) from free text |
| `ai.rank` | model | `items[, by]` | Reorder items by a natural-language criterion |
| `ai.judge` | model | `claim[, evidence]` | Adjudicate a claim → `Verdict` `{ choice, reasons }` |
| `ai.reason` | model | `ask[, ctx]` | Free-form reasoning over a context pack |
| `synth` | model | `claims[, format, cite]` | Synthesize a cited `Answer` from claims |
| `ai.rewrite` | model | `text[, style]` | Rewrite text in a requested style |

The model-backed ops carry a `Network` effect and require provider access (an LLM call is network
egress); the pure ops carry no effect and never pause for approval.

## Orchestration ops (the `flux-app` host only)

These are registered **only by the `flux-app` runtime host** (`flux run app.flux`), not the base engine
— a journey uses them to drive the event bus / channels. They add **no** new language node kinds.

| op | signature | description |
|---|---|---|
| `emit` | `event[, payload]` | Publish an event to the bus (fires any matching trigger's journey) |
| `send` | `channel, message` | Send a message to a named channel (a `cli` channel prints to stdout) |
| `ask` | `channel, message` | Send + return a correlation id (full request/response reply parking is a TODO) |
| `spawn` | `run[, input]` | Run a named journey to completion and return its result |

All four are Medium-risk / non-idempotent (`emit`/`spawn` fan out to other journeys, gated separately at
their own dispatch). See [`flux-lang-evolution.md`](../../../docs/designs/flux-lang-evolution.md) §6.

## Agent-loop ops (the self-hosted turn loop)

The turn loop is itself a Flux-Lang flow — `crates/flux-flow/assets/agent-loop.flux` — and these ops are
what let it call the model and run plans reflexively. They are how flux-lang self-hosts the agent loop:
`plan` re-enters the planner, `run_plan` re-enters the interpreter (over the same session + envelope), and
the evidence ops let the loop emit and read its own runtime observations and grade outcomes. Every one
still dispatches through the same `Executor` envelope — no bypass.

| op | signature | description |
|---|---|---|
| `plan` | `[feedback]` | Ask the model to emit a plan from the working conversation → a `Plan` `{kind: "plan"\|"chat"\|"error", text?, ast?, complete?}` (JSON). The model stays the planner; this wraps the compile step. |
| `run_plan` | `plan` | Execute an emitted plan in the **current** session → an `Outcome` `{transcript, result, steps, suspension?}`. Re-validated and run through the same approval+IO envelope; bounded by a reentry-depth cap. |
| `observe` | `kind[, data]` | Append an observation to the run's shared evidence log (the same log the runtime records `tool_call` markers into). |
| `evidence` | `[kind]` | Read observations back as a JSON array (filtered by `kind`, or the whole log) — so a flow can branch on what has happened so far. |
| `metrics` | | Summary counts from the evidence log: `{tool_calls, tool_errors, iterations}`. |
| `grade` | `criterion` | Evaluate a verifiable pass/fail `Criterion` (`command`/`file_content`/`all`) against the workspace → `"true"`/`"false"`, reusing the eval harness's own grader (`flux-eval`). |

**Visibility:** `plan`/`run_plan` are tagged to a never-surfaced `reflect` group, so the model never sees
them in its catalog — only a pre-authored flow (the agent loop, or `flux flow run`) can call them, and only
when a `LoopHost` is installed (the engine installs one per turn). `observe`/`evidence`/`metrics` are
ordinary builtins; `grade` is in the evidence-gated `eval` group.

On the **user-facing** surface these machinery ops are filtered out by default so the turn shows real
work, not plumbing. `flux run --show-loop` (or `FLUX_SHOW_LOOP=1`) reveals them so you can watch the
loop iterate; the REPL `/evidence` command prints the evidence log they write; and `flux loop
show`/`eject` reads or scaffolds the loop itself. See [docs/agent-loop.md](../../../docs/agent-loop.md).
