# Flux-Flow — Registered ops

The operations available through the engine's live `ToolRegistry`. The analyzer resolves authored
Flux calls against these contracts, and model-backed stages receive only the provider-native schemas
inside their capability ceiling. Operations are an **engine** concern (mostly provided by
`flux-tools`), not part of the Flux-Lang language — see
[`flux-lang/docs/reference.md`](../../flux-lang/docs/reference.md) for the language itself.

Every concrete call carries typed authorization requirements in addition to the risk/effect summary
shown here. Whole-plan preview and dispatch consume the same workspace, datasource, host, provider,
network, connection, process, secret, and semantic-action resources; approval cannot widen a policy
denial. See the repository's
[typed authority contract](../../../docs/designs/typed-authority-requirements.md).

## Registered ops quick reference

Ops are passed by name to `call`. Arguments are positional in the order shown;
optional arguments are in `[brackets]`.

| op | signature | risk | description |
|---|---|---|---|
| `read` | `path[, limit, offset]` | Low | Read one file (string path), a list of files (JSON array), or a glob pattern (string with `*`/`?`). Single-file: line-numbered view, paging via `offset`/`limit`. Multi-file/glob: sections headed `==> path <==`. Guidance returned for over-cap files. |
| `grep` | `pattern[, glob, literal, max_results, path]` | Low | Search by regex (supports `\b`, lookaheads); use `literal: true` for plain substring |
| `glob` | `pattern[, path]` | Low | List files matching a glob pattern (`*` crosses `/`) |
| `search` | `query[, limit]` | Low | Search the indexed datasource |
| `sources` | | Low | Enumerate the datasources in the index: per source, its entity types and record count |
| `web.fetch` | `url[, raw]` | Low | Read a page as a **document**: HTML → condensed markdown (boilerplate stripped), PDF → extracted text, other non-HTML raw, `raw: true` forces the raw body. Fetched pages become `web.page` records. Private/loopback blocked unless the `web` egress scope grants them |
| `web.crawl` | `url[, max_pages, max_depth, max_total_bytes]` | Low | Crawl a small site/section: from a seed, follow **same-host** links breadth-first (bounded — `max_pages` ≤ 50, `max_depth` ≤ 5, optional `max_total_bytes` content budget ≤ 512 KiB that stops the crawl early) → each page condensed to markdown + one `web.page` record per page. Same-host only; no robots.txt/sitemaps/JS (use `browser.*` for JS). Every hop guarded by the `web` egress scope |
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
| `web.search` | `query|queries[, max_results, providers]` | Low | First-party `websearch` plugin alias: Tavily when host auth is configured, otherwise DuckDuckGo; credentials never enter call input |
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
| `flow_run` | `name[, inputs]` | Medium | Run a stored flow by name from the flows home; `inputs` (a JSON object) are seeded as `$key` binds. Runs as an authored flow in the current session (needs a `LoopHost`) |
| `flow_render` | `source\|name[, view]` | Low | Render Flux-Lang as a syntax-highlighted SVG. Pass exactly one of inline `source` or the `name` of a stored flow. `view: "source"` (default) renders the highlighted source; `view: "tree"` renders the execution-path plan tree. Returns the SVG markup inline — for surfaces that can't highlight `.flux` themselves (READMEs, Slack, docs, chat) |
| `command.invoke` | `kind, name[, arguments]` | Low | Invoke a discovered command file (`kind: "command"`) or skill (`kind: "skill"`) — three independently-enforced, fail-closed gates: your policy permits `command.invoke` for this exact `kind:name` target, the target is discovered in this session, and its own frontmatter declares `agent-triggerable: true` (default false). A command expands `$ARGUMENTS`/`$1..$9` and returns the substituted body as prompt text (no nested execution, no side effects beyond the read); a skill returns its body verbatim. Any missing gate is a clean, recoverable refusal. Evidence-gated on group `agent_invoke` (D-187, absorbs C-93) |
| `skill.load` | `name` | Low | Pull one skill's full body into context by exact `name`, from the `<available-skills>` catalog surfaced when the opt-in model-invoked skill mode is on (`--skills-model-invoked` / `[skills] model_invoked`, D-188). Only advertised when that catalog is non-empty for the session. Loading is idempotent and persists: the skill behaves like an explicitly `--skill`-activated one for the rest of the session (its body is re-injected on every later turn). Excludes skills declaring `disable-model-invocation: true`, which never enter the catalog |

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

The turn loop is the authored Flux-Lang flow in `crates/flux-flow/assets/agent-loop.flux`. Its model
boundaries return stage-owned typed values or provider-native operation calls; a model never emits a
Flux program. Host-built action batches separate proposal from execution, and every leaf operation
still dispatches through the same `Executor` envelope.

| op | signature | description |
|---|---|---|
| `detect_intent` | | Run the typed intent stage over the current conversation → `{kind, intent, families, operations, state}`. Capability families are intersected with the live, wired, permitted registry; signals never grant authority. |
| `explore` | `state[, decision, report]` | Continue the bounded provider-native stage ledger. Gather-safe calls execute through `Executor`; effectful calls are captured. Returns a typed step with `kind: "chat"\|"decision"\|"batch"\|"error"`, the durable `state`, and the corresponding `text`, `question`, or host-built `batch`. A decision or execution report closes the matching pending native call. |
| `approve_batch` | `batch` | Validate the live operation schemas, compute aggregate risk, request one batch approval, and return an `ApprovalReceipt`. The opaque receipt is bound to the exact batch, session, caller/authority context, and policy context. |
| `execute_batch` | `batch, receipt` | Consume a matching one-shot receipt and execute the ordered actions through `Executor`. Missing, changed, stale, reused, denied, or cross-context receipts fail closed. Returns an `ExecutionReport`; after one action fails, later actions are marked skipped. |
| `present_results` | `step` or `approval` | Render a terminal chat/error/decision step or an approval denial into user-facing text without giving that text execution semantics. |
| `ai_segment` | `goal, tools, max_rounds[, until]` | Run a bounded adaptive segment inside a deterministic flow. The required authored `max_rounds` is the segment's exact provider-call ceiling (it is not clamped by the normal agent default). The authored `tools` list is a hard live capability ceiling; reads gather evidence, effects use the same batch path, and the result is returned as `{result, state[, decision]}`. Provider usage folds into the enclosing turn. |
| `op.register` | `source, scope[, replace, expose]` | Register exactly one top-level Flux-Lang composite `op` for later reuse. `scope` is `turn`, `session`, `project`, or `global`; project/global writes are guarded filesystem writes, and all registered inner ops still dispatch through the normal envelope. |
| `observe` | `kind[, data]` | Append an observation to the shared evidence log. The adaptive loop records stage transitions, intent, action-batch proposal/approval/execution, and turn execution reports. |
| `evidence` | `[kind]` | Read observations back as a JSON array (filtered by `kind`, or the whole log). |
| `metrics` | | Summary counts from the evidence log: `{tool_calls, tool_errors, iterations}`. |
| `grade` | `criterion` | Evaluate a verifiable pass/fail `Criterion` (`command`/`file_content`/`all`) against the workspace → `"true"`/`"false"`, reusing the eval harness's grader (`flux-eval`). |

The standard artifacts are `IntentSet`, `DecisionRequest`, `ActionBatch`, `ApprovalReceipt`, and
`ExecutionReport`, but custom model stages are ordinary operations and may define unrelated input and
output schemas. Config stages live under `[agent.stages.<name>]`; SDK callers can register a typed
`stage_fn::<I, O, _, _, _>(...)`. A model stage may call only explicitly declared, statically
gather-safe tools.

**Visibility:** the loop machinery is tagged to a never-surfaced host group. A model cannot invoke
`detect_intent`, batch approval/execution, or `present_results`; only an analyzed authored flow may do
so when a `LoopHost` is installed. `op.register`, `flow_list`, and `flow_run` remain model-facing root
operations when their surface installs them. `flow_run` enters the authored-flow host in the current
session and keeps the same capability and safety envelope.

User-facing surfaces hide machinery operations by default. `flux run --show-loop` (or
`FLUX_SHOW_LOOP=1`) reveals them, `--trace-loop` traces structural nodes, `/evidence` prints the audit
observations, and `flux loop show`/`eject` prints or scaffolds the preset. An ejected file takes effect
only when selected explicitly with `--loop` or `[agent] loop = "..."`. See
[docs/agent-loop.md](../../../docs/agent-loop.md).
