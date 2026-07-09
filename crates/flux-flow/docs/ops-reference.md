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
| `web_fetch` | `url[, raw]` | Low | Read a page as a **document**: HTML → condensed markdown (boilerplate stripped), non-HTML raw, `raw: true` forces the raw body. Fetched pages become `web.page` records. Private/loopback blocked unless the `web` egress scope grants them |
| `html_to_markdown` | `html` | Low | Pure (no egress): condense an HTML string to readable markdown; composes with `http.request` |
| `http.request` | `url[, method, headers, body, timeout]` | Medium | Make an arbitrary HTTP(S) request (any method/headers/body) → status + headers + capped body; non-2xx is a result. Header values may be `{"$secret": "ENV"}`. Private/loopback blocked unless the `web` egress scope grants them |
| `browser.open` | `[url]` | Medium | Open a headless-Chromium session (evidence-gated on a discoverable browser) → `session` id + a non-visual page digest (condensed content + `e<N>` action refs). Every subrequest is guarded by the `web` egress scope |
| `browser.goto` | `session, url` | Medium | Navigate a session; returns a delta of what changed |
| `browser.snapshot` | `session[, view]` | Low | Re-observe a session as a digest (`view`: full \| actions \| content) |
| `browser.act` | `session, action[, ref, value, full]` | Medium | Act on a ref (click/type/fill/select/press/scroll/goto/back) → delta digest; `full` for a whole digest |
| `browser.close` | `session` | Low | Close a session + its Chromium child |
| `write` | `path, content` | Medium | Write (create/overwrite) a file |
| `edit` | `path, old_string, new_string[, replace_all]` | Medium | Replace a string in a file (must match exactly once unless `replace_all`); if the exact text isn't found, progressively looser matching is tried (trailing whitespace → indentation drift → first/last-line anchor) and the result reports which strategy matched |
| `patch` | `path, edits` | Medium | Apply several line-anchored edits in one call; each edit is `{op, line, end_line?, text?}` where op is `insert_before`, `insert_after`, `replace_range`, or `delete_range`; ALL line numbers refer to the original file |
| `append` | `path, content` | Low | Append to a file (creates it and parent dirs if absent); lower-risk than `write` |
| `read_many` | `paths` | Low | Read several files at once (each section headed `==> path <==`); prefer single `read` when you need to embed a file's text into a later string |
| `task` | `role, task` | Medium | Delegate to a sub-agent role |
| `bash` | `command[, timeout_secs]` | High | Run a shell command |
| `proc.run` | `program[, args, timeout_secs]` | High | Run one argv-only process in the workspace root (no shell, env cleared by `flux-system`) |
| `file_stat` | `path` | Low | File metadata: size, line count, mtime (replaces `wc -l`, `stat`, `ls -la`) |
| `path_exists` | `path` | Low | Returns `"true"`/`"false"` — use with `when`/`unless` to branch on file presence |
| `sqlite_query` | `db, sql[, params]` | Low | Read-only SQLite query (SELECT/PRAGMA only) |
| `web_search` | `query[, max_results]` | Low | Tavily web search — requires `TAVILY_API_KEY` env var |
| `now` | | Low | Current wall-clock time: unix seconds + UTC string (replaces `date`) |
| `cwd` | | Low | Absolute path of the workspace root (replaces `pwd`) |
| `sys_info` | | Low | Host metadata: os, arch, family, hostname (replaces `uname`) |
| `cargo_check` | `[package, manifest_path, args]` | Medium | `cargo check` (type-check only, no codegen; `manifest_path` targets nested workspaces) |
| `cargo_build` | `[package, manifest_path, release, args]` | Medium | `cargo build` (`manifest_path` targets nested workspaces) |
| `cargo_test` | `[package, manifest_path, filter, args]` | Medium | `cargo test` (`manifest_path` targets nested workspaces) |
| `cargo_clippy` | `[package, manifest_path, deny_warnings, args]` | Medium | `cargo clippy` (`manifest_path` targets nested workspaces) |
| `cargo_fmt` | `[package, manifest_path, check]` | Medium | `cargo fmt` (pass `check: true` to only verify; `manifest_path` targets nested workspaces) |
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
| `flow_list` | | Low | List reusable flows and composite ops under `.flux/flows` / `~/.flux/flows` (and the legacy `.flux/ops` / `@global_ops`) — each with its description and params |
| `flow_run` | `name[, inputs]` | Medium | Run a stored flow by name from the flows home; `inputs` (a JSON object) are seeded as `$key` binds. Runs in the current session by re-entering `run_plan` (needs a `LoopHost`) |

`write`, `edit`, `patch`, `append`, `task`, `bash`, `proc.run`, and the toolchain ops (`cargo_*`, `go_*`,
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
| `dedupe` | pure | `items[, by]` | Remove duplicates (whole-value, or by a dotted field path), first-seen order |
| `sort` | pure | `items[, by, order]` | Stable sort by a dotted field path (or natural); `order` = `asc`/`desc` |
| `top` | pure | `items, n` | The first `n` items |
| `skip` | pure | `items, n` | Drop the first `n` items |
| `merge` | pure | `lists` | Concatenate an array-of-arrays into one array |
| `map` | pure | `items, path|expr[, vars]` | Project each item by dotted path or an `expr` formula with `it` bound |
| `filter` | pure | `items[, where, vars, by, equals]` | Keep items by an `expr` predicate or by dotted field/equality |
| `flatten` | pure | `items[, depth]` | Flatten nested arrays up to `depth` levels |
| `join` | pure | `items[, sep]` | Stringify and join items into plain text |
| `split` | pure | `s[, sep, trim]` | Split text into a JSON array of strings |
| `sum` | pure | `items[, path]` | Sum numbers, optionally plucked from a dotted path |
| `count_by` | pure | `items, path` | Count items by dotted path, sorted by count desc then key |
| `group_by` | pure | `items, path` | Group items by dotted path in first-seen key order |
| `any` | pure | `items[, where, vars]` | `"true"` when any item is truthy or matches an `expr` predicate |
| `all` | pure | `items[, where, vars]` | `"true"` when all items match; empty lists are vacuously true |
| `has` | pure | `items, value` | `"true"` when the array contains `value` by JSON equality |
| `pick` | pure | `items, keys` | Keep only listed object keys; accepts one object or an array of objects |
| `omit` | pure | `items, keys` | Remove listed object keys; accepts one object or an array of objects |
| `merge_obj` | pure | `objects` | Shallow-merge objects left to right; later keys win |
| `coalesce` | pure | `values[, default]` | First value that is neither `null` nor `""`; otherwise `default` or `null` |
| `keys` | pure | `item` | Object keys in deterministic order |
| `values` | pure | `item` | Object values in deterministic key order |
| `regex_match` | pure | `s, pattern` | `"true"`/`"false"` for a bounded, ReDoS-free Rust regex match |
| `regex_extract` | pure | `s, pattern[, group, all]` | First regex match, `null`, or all requested captures as a JSON array |
| `cite` | pure | `claims` | A markdown citation list, one line per claim |
| `len` | pure | `items` | Count of an array's items (or a string's characters) |
| `first` | pure | `items` | The first item of an array (or `null`) |
| `last` | pure | `items` | The last item of an array (or `null`) |
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

The loop is **phased** (A-14, design [`multipass-agent-loop.md`](../../../docs/designs/multipass-agent-loop.md)):
one **orient** `plan` call (a three-way contract — prose chat, the full execution plan, or a small
read-only `gather: true` plan + `brief`), a bounded **gather** pass (`repeat 3`, skipped entirely when
orient already settled), then the standard **execute** plan/run/revise pass (`repeat 25`, unchanged
guards). `phase` (`"orient"`/`"gather"`/`"execute"`) selects the planner's per-phase instruction segment;
`settled` on the returned `Plan` is `""` only for an accepted `gather: true` plan (gating the gather
pass's `until $settled`), truthy otherwise. A `gather: true` plan is enforced, not trusted — effect-clean
and capped at ~12 call nodes — and the execute phase always rejects a further `gather: true` emission
(the budget is spent). If the gather budget exhausts before settling, the leftover gather plan simply
runs as the execute pass's first iteration.

| op | signature | description |
|---|---|---|
| `plan` | `[feedback, phase]` | Ask the model to emit a plan from the working conversation → a `Plan` `{kind: "plan"\|"chat"\|"error", text?, ast?, complete?, settled}` (JSON). `phase` is `"orient"`/`"gather"`/`"execute"` — absent or unrecognized behaves as `"execute"` (byte-compatible with a phase-less/pre-A-14 caller, e.g. an ejected loop). `complete` is the model's completion directive (`{primer?, instructions}`) or `null`. The model stays the planner; this wraps the compile step. |
| `run_plan` | `plan` | Execute an emitted plan in the **current** session → an `Outcome` `{transcript, result, steps, suspension?, failure}`. Re-validated and run through the same approval+IO envelope; bounded by a reentry-depth cap. `failure` is `null` when this round ran clean; otherwise a reified mid-plan halt (design [`multipass-agent-loop.md`](../../../docs/designs/multipass-agent-loop.md) Part 2) — `{node, stmt, op, kind, fatal, message, plan, completed[]}` — that a corrected re-emission fast-forwards the matching completed prefix of (A-16/A-17; never propagated as `Err`). When the plan carried `complete` and ran to success, the **next** `plan` call renders the final message from the results (a toolless model call) and returns it as `{kind: "chat"}` — the complete fast-path. |
| `op.register` | `source, scope[, replace, expose]` | Register exactly one top-level Flux-Lang composite `op` for later reuse. `scope` is `turn`, `session`, `project`, or `global`; project/global writes are guarded filesystem writes, and all registered inner ops still dispatch through the normal envelope. |
| `observe` | `kind[, data]` | Append an observation to the run's shared evidence log (the same log the runtime records `tool_call` markers into). The loop itself emits `loop.phase` (at every `plan` entry, payload `{phase}`), `flow.brief` (the moment a `brief` is accepted, payload `{goal, needs}`), `turn.gather` (each gather round's `Outcome`), `turn.iteration` (each clean execute round's `Outcome`), and `turn.revision` (an execute round whose `Outcome.failure` was set — A-17). `run_plan` itself streams (not through this log) `flow.plan` (the compiled plan tree — `resumed`/`gather`/`phase` flags let a surface render it correctly) and, on a halt, `flow.halt` (`{step, of, op, kind, fatal}`, a real-time cue distinct from the fed-back transcript text). |
| `evidence` | `[kind]` | Read observations back as a JSON array (filtered by `kind`, or the whole log) — so a flow can branch on what has happened so far. |
| `metrics` | | Summary counts from the evidence log: `{tool_calls, tool_errors, iterations}`. |
| `grade` | `criterion` | Evaluate a verifiable pass/fail `Criterion` (`command`/`file_content`/`all`) against the workspace → `"true"`/`"false"`, reusing the eval harness's own grader (`flux-eval`). |

The brief accepted alongside a `gather: true` plan is **host-carried for the rest of the turn**: it is
prepended to every subsequent `plan` call's feedback message (not just the immediate next round), so a
multi-round gather — or the execute phase that follows it — never loses the thread. It resets at the
start of the next turn.

**Visibility:** `plan`/`run_plan` are tagged to a never-surfaced `reflect` group, so the model never sees
them in its catalog — only a pre-authored flow (the agent loop, or `flux flow run`) can call them, and only
when a `LoopHost` is installed (the engine installs one per turn). `op.register` is a model-facing root op,
available only when the engine installs a composite registrar. `observe`/`evidence`/`metrics` are ordinary
builtins; `grade` is in the evidence-gated `eval` group. `flow_list`/`flow_run` (registered by the CLI
host's `flux_tools::register_flows`, not base `register_builtins`) are **model-facing**; `flow_run` also
needs a `LoopHost`, since it re-enters `run_plan` to run the resolved flow.

On the **user-facing** surface these machinery ops are filtered out by default so the turn shows real
work, not plumbing. `flux run --show-loop` (or `FLUX_SHOW_LOOP=1`) reveals them so you can watch the
loop iterate; the REPL `/evidence` command prints the evidence log they write; and `flux loop
show`/`eject` reads or scaffolds the loop itself. See [docs/agent-loop.md](../../../docs/agent-loop.md).
