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
| `search` | `query[, source, entity, harness, limit]` | Low | Search the indexed datasource. `harness` appears only where the host opted into local coding-harness history, and restricts to one of them |
| `sources` | | Low | Enumerate the datasources in the index: per source, its entity types and record count |
| `get` | `source, entity, id` | Low | Fetch one datasource record in full by its address |
| `list` | `source[, entity, offset, limit]` | Low | List one source's records (optionally one entity type), paged |
| `relation` | `source, entity, id[, rel]` | Low | Follow a record's typed relations to the linked records (optionally one relation name) |
| `batch_get` | `source, entity, ids` | Low | Fetch several records of one entity, from one source, in one call |
| `web.fetch` | `url[, raw]` | Medium | Read a page as a **document**: HTML → condensed markdown (boilerplate stripped), PDF → extracted text, other non-HTML raw, `raw: true` forces the raw body. Fetched pages become `web.page` records. Private/loopback blocked unless the `web` egress scope grants them |
| `web.crawl` | `url[, max_pages, max_depth, max_total_bytes]` | Medium | Crawl a small site/section: from a seed, follow **same-host** links breadth-first (bounded — `max_pages` ≤ 50, `max_depth` ≤ 5, optional `max_total_bytes` content budget ≤ 512 KiB that stops the crawl early) → each page condensed to markdown + one `web.page` record per page. Same-host only; no robots.txt/sitemaps/JS (use `browser.*` for JS). Every hop guarded by the `web` egress scope |
| `html_to_markdown` | `html` | Low | Pure (no egress): condense an HTML string to readable markdown; composes with `http.request` |
| `http.request` | `url[, method, query, headers, body, timeout]` | Medium | Make an arbitrary HTTP(S) request (any method/headers/body) → the **record** `{status, headers, body}`: `status` a number, `headers` a map keyed by the response header name (a repeat is joined with `, `), `body` the parsed JSON when the response is a JSON object or array and the raw capped text otherwise. Select a field directly — `$resp.body.data.id`; for a hyphenated header use `pick({items: $resp.headers, keys: ["content-type"]})`. Non-2xx is a result, and an HTML/empty/truncated body still yields the record. `query` is a record of scalars, each **percent-encoded per RFC 3986** before it is appended — never format a value into `url` yourself; a `null` field is omitted, `false`/`0` are sent, a nested value or a key already in `url` is an error. Header and query values may be `{"$secret": "ENV"}`, for env names the operator allowlisted — and an allowlist entry may scope the secret to particular destination hosts, principals, or header-vs-query placement (`NAME;to=api.example.com;in=header`), checked against the address the egress guard vetted; a credential the response echoes back is redacted in the record. Private/loopback blocked unless the `web` egress scope grants them |
| `browser.open` | `[url]` | Medium | Open a headless-Chromium session (evidence-gated on a discoverable browser) → `session` id + a non-visual page digest (condensed content + `e<N>` action refs). Every subrequest is guarded by the `web` egress scope |
| `browser.goto` | `session, url` | Medium | Navigate a session; returns a delta of what changed |
| `browser.snapshot` | `session[, view]` | Medium | Re-observe a session as a digest (`view`: full \| actions \| content) |
| `browser.act` | `session, action[, ref, value, full]` | Medium | Act on a ref (click/type/fill/select/press/scroll/goto/back) → delta digest; `full` for a whole digest |
| `browser.close` | `session` | Medium | Close a session + its Chromium child |
| `write` | `path, content` | Medium | Write (create/overwrite) a file |
| `edit` | `path, old_string, new_string[, replace_all]` | Medium | Replace a string in a file (must match exactly once unless `replace_all`); if the exact text isn't found, progressively looser matching is tried (trailing whitespace → indentation drift → first/last-line anchor) and the result reports which strategy matched |
| `patch` | `path, edits` | Medium | Apply several line-anchored edits in one call; each edit is `{op, line, end_line?, text?}` where op is `insert_before`, `insert_after`, `replace_range`, or `delete_range`; ALL line numbers refer to the original file |
| `append` | `path, content` | Medium | Append to a file (creates it and parent dirs if absent); smaller blast radius than `write`, same approval tier |
| `read_many` | `paths` | Low | Read several files at once (each section headed `==> path <==`); prefer single `read` when you need to embed a file's text into a later string |
| `task` | `role, task` | Medium | Delegate to a sub-agent role |
| `consult` | `question[, context, model]` | Medium | Ask a DIFFERENT model for a second opinion — pure advice, no tools (only advertised once `[consult] model` is configured) |
| `bash` | `command[, timeout_secs]` | High | Run a shell command |
| `proc.run` | `program[, args, timeout_secs]` | High | Run one argv-only process in the workspace root (no shell, env cleared by `flux-system`) |
| `file_stat` | `path` | Low | File metadata: size, line count, mtime (replaces `wc -l`, `stat`, `ls -la`) |
| `path_exists` | `path` | Low | Returns `"true"`/`"false"` — use with `when`/`unless` to branch on file presence |
| `sqlite_query` | `db, sql[, params]` | Low | Read-only SQLite query (SELECT/PRAGMA only) |
| `web.search` | `query|queries[, max_results, providers]` | Low | First-party `websearch` plugin alias: Tavily when host auth is configured, otherwise DuckDuckGo; credentials never enter call input |
| `now` | | Low | Current wall-clock time: unix seconds + UTC string (replaces `date`) |
| `cwd` | | Low | Absolute path of the workspace root (replaces `pwd`) |
| `home_dir` | | Low | The current user's home directory (`$HOME`) — build `~/.flux/sessions.db`-style absolute paths without shelling out |
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
| `git_merge` | `branch[, no_ff]` | High | Merge a ref into the current branch (`no_ff` forces a merge commit); a conflict is a recoverable error naming the conflicting files — the merge is aborted and the tree restored, never left half-merged. Refuses outright if a merge is already in progress, and aborts nothing in that case: the in-flight resolution may be uncommitted work |
| `git_revert` | `commit[, mainline]` | High | Revert a commit by appending its inverse (`mainline`, usually 1, for a merge) — a new commit undoes the target, never a reset; requires a clean tree, and a conflicted revert is aborted and left clean, naming the conflicting files |
| `git_push` | `[branch, remote]` | Medium | Push to remote |
| `git_checkout` | `branch[, create]` | Medium | Switch/create branch |
| `git_branch` | `name[, delete]` | Medium | Create a branch without switching to it, or safe-delete one (`-d` — git refuses unmerged work and the checked-out branch) |
| `git_unstage` | `paths` | Medium | Unstage files |
| `git_hunks` | `path[, context]` | Low | List one file's individually stageable unstaged hunks, each with a stable id |
| `git_stage_hunks` | `path, hunks[, context]` | Medium | Stage only the named hunks of one file (the `git add -p` equivalent) |
| `git_worktree_enter` | | High | Move this context into an isolated temp git worktree (clean `main` only; generated `flux/worktree/*` branch) |
| `git_worktree_leave` | | High | Merge the worktree back into `main` (`--no-ff`, trial-merge guarded), clean up, restore the original root |
| `flow_list` | | Low | List reusable flows and composite ops under `.flux/flows` / `~/.flux/flows` (and the legacy `.flux/ops` / `@global_ops`) — each with its description and params |
| `flow_run` | `name\|path[, inputs]` | Medium | Run exactly one stored-flow `name` or workspace-relative `.flux` `path`; path source is reread for every call, `inputs` are seeded as `$key` binds, and the authored flow is validated against the current operation catalog before it runs in the guarded session. Returns a route receipt with the resolved path, flow name, and seeded input keys (needs a `LoopHost`) |
| `flow_render` | `source\|name[, view]` | Low | Render Flux-Lang as a syntax-highlighted SVG. Pass exactly one of inline `source` or the `name` of a stored flow. `view: "source"` (default) renders the highlighted source; `view: "tree"` renders the execution-path plan tree. Returns the SVG markup inline — for surfaces that can't highlight `.flux` themselves (READMEs, Slack, docs, chat) |
| `command.invoke` | `kind, name[, arguments]` | Low | Invoke a discovered command file (`kind: "command"`) or skill (`kind: "skill"`) — three independently-enforced, fail-closed gates: your policy permits `command.invoke` for this exact `kind:name` target, the target is discovered in this session, and its own frontmatter declares `agent-triggerable: true` (default false). A command expands `$ARGUMENTS`/`$1..$9` and returns the substituted body as prompt text (no nested execution, no side effects beyond the read); a skill returns its body verbatim. Any missing gate is a clean, recoverable refusal. Evidence-gated on group `agent_invoke` (D-187, absorbs C-93) |
| `skill.load` | `name` | Low | Pull one skill's full body into context by exact `name`, from the `<available-skills>` catalog surfaced when the opt-in model-invoked skill mode is on (`--skills-model-invoked` / `[skills] model_invoked`, D-188). Only advertised when that catalog is non-empty for the session. Loading is idempotent and persists: the skill behaves like an explicitly `--skill`-activated one for the rest of the session (its body is re-injected on every later turn). Excludes skills declaring `disable-model-invocation: true`, which never enter the catalog |
| `review.normalize` | `findings` | Low | Pure: parse raw reviewer output into well-formed findings, each with a stable fingerprint (category + file + line + normalized title), quarantining malformed entries as human-readable `gaps` rather than dropping them. Returns `{findings, gaps}`; does not dedupe or rank |
| `review.aggregate` | `findings[, files, reviewers]` | Low | Pure: normalize, dedupe by fingerprint (counting distinct-reviewer `agreement`), then rank by severity → confidence → agreement with a fingerprint tiebreak, so the ordering is byte-identical across runs. Returns `{summary, findings, checked_files, reviewers, gaps}` |
| `schedule_wakeup` | `prompt, in_secs[, context]` | Medium | Register a future wake-up on **this** session: after `in_secs`, the session resumes with `prompt` as if it were a new message, replaying any `context` captured now (contained, never re-read as fresh instructions). Ends a turn while something else is still in flight instead of blocking. Requires approval; bounded by a maximum horizon and a per-session pending cap. Only registered when `[wakeup] enabled` |
| `endpoint.discover` | `product[, query, cluster, namespace, limit]` | Low | Fan out to provider plugins for live service endpoints of a `product` (kubernetes, postgres, mysql, prometheus, loki, grafana, alertmanager) → **weak references** (URL + labels, never a secret) |
| `endpoint.list` | | Low | The endpoint references this session knows (discovered or config-bound), with owner and last health — weak references only |
| `endpoint.info` | `id` | Low | One endpoint reference in full by id (e.g. `@endpoint/monitoring-prometheus`): URL, product, protocol, labels, owner, health; never a secret |
| `endpoint.select` | `id` | Low | Bind a discovered endpoint by id and return its weak reference, to reuse across turns; the host resolves it and injects the credential when a call runs |
| `endpoint.import` | `id` | Medium | Persist an endpoint reference to the local endpoints store so it survives the session — the weak reference only (URL + credential *location*); the credential is re-resolved live each session |
| `host.list` | | Low | The named execution-substrate bindings registered this session (`[[host]]` config + hosts store): backend kind, address, availability — weak references only |
| `host.info` | `id` | Low | One host binding in full by name: backend kind, address, availability, labels and a credential *presence* marker; never a value |
| `host.probe` | `id` | Low | The backend's side-effect-free identity check: substrate identity (kind, workspace, confinement, remotely_reported) and, for a remote backend, the negotiated protocol version — nothing executes on the substrate |
| `flux_reload` | | High | **`--dev` only**: recompile `flux-cli` in place. The new binary lands on disk but this session keeps the old one, so it returns instructions to exit and re-run with `--resume`; it never replaces the running process (C-57) |

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

**Ops that *select* an existing value return a bare string unquoted** (C-236, fixing C-235):
`regex_extract` (single match), `first`, `last` and `coalesce` bind the string itself, so
`regex_extract` of a URL feeds straight into an op that parses one. Anything structured (object,
array, number, bool, `null`) is still its compact JSON encoding, which the runtime's string-leaf
re-parse rule reads back — so `split`, `keys`, and `regex_extract` with `all: true` are unchanged.

## Second opinion (group `consult`)

`consult` (A-96) is deliberately separate from the cognition pack above: it does not reuse the
agent's own provider/model, it resolves a **different** one per call — an op-argument
`provider/model` spec, else the configured `[consult] model` default, else the agent's own model —
through the same routing `-m`/`--model` uses (subscription providers included). It lives in
`flux-cognition` too but is registered independently of `CognitionPack`, and is only advertised
when `[consult] model` is configured (evidence-gated group `consult`, so an unconfigured workspace
never sees it — the A-95 cache-stability lesson: the surfacing decision is made once at agent
assembly and never churns mid-session).

| op | kind | signature | description |
|---|---|---|---|
| `consult` | model | `question[, context, model]` | Ask another model for a second opinion; returns its answer as text |

It is pure: exactly one model call, no tools, no filesystem/process authority, and no network
authority beyond that one call (`Network` effect + `Provider` access, same pair as the cognition
ops above). The reply is untrusted model output from elsewhere, so it returns wrapped in the same
containment tag the knowledge-injection path uses (A-21), and the call is attributed to the
calling turn's usage exactly like every other model-stage call, subject to a per-turn call cap
(`[consult] max_calls`, default 2) so it stays a cheap escape valve rather than an unbounded spend.

## Fleet ops (outbound A2A dispatch)

`task` delegates to a **local** sub-agent and awaits it. The `fleet.*` ops (A-116, wired into the
CLI catalog by A-131) are the half `task` cannot express: hand work to a **remote** `flux serve`
worker over A2A *without* waiting, then poll or stop it. That is what lets a coordinator hold ten
workers in flight and reconcile them later.

| op | signature | risk | description |
|---|---|---|---|
| `fleet.dispatch` | `worker, task[, role, context_id]` | Medium | Send a task to a remote worker and return its `task_id` without waiting. A worker that answers synchronously returns `task_id: null` plus its answer, rather than an id that would be polled forever |
| `fleet.status` | `worker, task_id` | Low | Read a dispatched task's current state → `{task_id, state, terminal, text}` |
| `fleet.cancel` | `worker, task_id` | Medium | Stop a dispatched task. An already-finished task reports that it was not cancelable |
| `fleet.isolate` | `item` | High | Create branch `impl/<item>` in its own git worktree off the current clean HEAD and return `{worktree, branch, base_commit}` — a per-item checkout for ONE local worker. Unlike `git_worktree_enter` it does not move the caller's own working root, so a coordinator can call it once per item in a single turn. Refuses a dirty base, an existing `impl/<item>`, and nesting inside a worktree session; removing the worktree afterwards is the caller's job, because it holds the worker's unmerged diff |
| `fleet.start` | `item[, worktree, context_id, model]` | High | Start a flux worker for one board item as a guarded child process and return `{worker_id, endpoint, context_id, runtime, state}`. `worktree` confines it to that checkout (cwd + sandbox writable set) and may sit outside the workspace root, which is what `fleet.isolate` returns; the returned `context_id` resumes the same worker session on a later `fleet.dispatch`. Refuses when the sandbox is active without network — a wrapped worker binds inside its own netns and nothing could reach it |
| `fleet.worker_status` | `worker_id` | Low | Worker liveness → `{state: starting\|live\|dead, live, endpoint, context_id, exit_code, detail}`. A dead worker reports no endpoint and carries the tail of its own output. This is the worker, not a task — for a task use `fleet.status` |
| `fleet.stop` | `worker_id` | Medium | Stop a worker started by `fleet.start`, terminating its process group. An unknown worker id is an error; an externally managed worker refuses |
| `fleet.agents` | `[limit]` | Low | On an explicitly attached native Fleet main only, list bounded durable worker admissions and current statuses without requiring known worker ids. This reads `.flux/fleet/state.json`; it does not inspect the transient A2A/process workers used by the other operations in this table |

An attached native Fleet main replaces the colliding transient `fleet.status` / `fleet.cancel`
implementations and installs the following closed coordinator service catalog. These operations call
the same versioned JSON Board/Fleet services as the CLI; they are absent from ordinary agents and
story workers. `task` remains the only non-Board/Fleet model-facing operation and its child catalog
is independently reduced to read-only research. `[main].research_loop` forces a second
operator-authored loop onto every such child, so role defaults cannot restore generic planning.

| op | signature and risk | description |
|---|---|---|
| `board.show` | `{}` · Low | Show the authoritative workspace Board and planning documents |
| `board.get` | `id` · Low | Read one exact namespaced item from the authoritative workspace Board |
| `board.next` | `[limit]` · Low | List dependency-satisfied ready Board items in deterministic priority order |
| `board.check` | `{}` · Low | Validate workspace Board configuration and story contracts |
| `board.start` | `id[, if_revision, idempotency_key]` · Medium | Move one authoritative item to `in-progress` |
| `board.block` | `id, reason[, if_revision, idempotency_key]` · Medium | Block one authoritative item and record why |
| `board.unblock` | `id[, if_revision, idempotency_key]` · Medium | Return one blocked item to ready |
| `board.comment` | `id, text[, if_revision, idempotency_key]` · Medium | Append a durable item comment |
| `board.evidence` | `id, text[, if_revision, idempotency_key]` · Medium | Append structured item evidence |
| `fleet.status` | `{}` · Low | Read the bounded durable Fleet lifecycle snapshot; historical turn and wave bodies are excluded |
| `fleet.schedule` | `{}` · Low | Read the dependency-aware native Fleet schedule derived from the Board |
| `fleet.run` | `items[, prepare_only, if_revision, idempotency_key]` · Medium | Prepare and launch a native Fleet wave for 1–10 exact dependency-satisfied Board refs |
| `fleet.message` | `target, message[, wait, if_revision, idempotency_key]` · Medium | Deliver an acknowledged message to an admitted native Fleet agent |
| `fleet.cancel` | `target[, if_revision, idempotency_key]` · Medium | Cancel one exact durable native Fleet worker or wave |
| `fleet.resume` | `target[, if_revision, idempotency_key]` · Medium | Resume one exact native Fleet target from durable admitted state |

**Egress posture.** `worker` is a caller-supplied argument, not configuration, so it is
model-reachable and treated as such:

- Every call resolves the endpoint through `flux_system::net::guard_url_scoped` **before** any
  request, in both directions.
- `permission_subjects` reports the worker's **origin** (`https://worker-1.internal:8787`) — never
  `*`. An endpoint the op cannot parse reports **no** subject, which forces approval instead of
  matching a broad grant.
- The ops carry no standing private-network grant. They are *not* folded into the `[private_net]
  web` scope, which names the native web family; only the blanket `--allow-private-net` /
  `FLUX_ALLOW_PRIVATE_NET` override admits a private or loopback worker.

`fleet.status` is deliberately **not** `Idempotent` — that word would license the op cache to serve
a stored result instead of executing, and observing the change since the last poll is the whole
point. `fleet.cancel` is `Conditional`: a repeat answers `TaskNotCancelable` rather than acting
again.

The ops are force-on (group `fleet`, empty `surface_when`): the worker address is per-call, so there
is no workspace signal that could gate them honestly. `.flux/groups.toml` can still reassign or gate
the group. A worker behind `flux serve`'s required bearer token is not yet reachable — the token is
operator configuration that does not exist yet.

`fleet.agents` is the exception to that force-on catalog: it is installed only for the main agent
started by `flux tui --fleet`. Its result is capped at 100 worker records, reports the untruncated
total, and intentionally omits worker instructions and turn bodies. The validated attachment
pre-authorizes this read, while an operator-authored deny rule still wins.

## Work board ops (`<domain>.list` / `.get` / `.create` / `.transition` / `.claim` / `.comment` / `.record_dispatch` / `.query` / `.comments` / `.reassign` / `.record_evidence`)

A `WorkBoard` (A-113) is a typed item state machine behind a swappable backend, separate from the
read-only datasource catalogue. A program binds one with a first-class `board` declaration and the
host generates eleven operations under the declaration's name — so `board board`
yields `board.list` … `board.record_evidence`. Seven of them write, and each reports a concrete
`board:<domain>/item/<id>` permission subject (`board:<domain>/item/new` for `create`); `transition` validates
the edge before writing, so an illegal edge errors and performs no write. `record_dispatch` (A-130)
binds an item to the worker running it — the `runner` address and the worker-minted `task_id` — which
is what makes the board a run registry rather than only a task list; it writes those two fields and
nothing else, so `transition` stays the single entry point into the state machine.

**Both edges into `ready` are retries** (C-240): `failed → ready` and `blocked → ready` each
increment `attempts` — so a rework budget cannot be laundered by cycling through `blocked` — and each
clears `runner`/`task_id`, because the run they name is dead and the next sweep would otherwise chase
it. `assignee` is never cleared by either: the holder outlives one run. `reassign` (C-240) is the one
path that *does* move the holder — a deliberately forcible takeover for when the holder is gone,
where `claim` conflicts — and it drops the previous worker's `runner`/`task_id` on the same grounds.
`record_evidence` (C-240) appends a weak locator for an artifact produced against the item, either a
`url` or an `entity` + `entity_id` pair (`commit`/`<sha>`); naming both, or neither, is an error, and
re-recording an artifact the item already cites changes nothing. Neither op moves the state machine.

`list` renders prose for a human and pages with a cursor. **`query` (C-236) is its machine-readable
sibling**: one page as a bare JSON array of typed rows under a real `output_schema`, so
`each $item in board.query({…})` binds `id`/`state`/`runner`/`task_id`/`depends_on`/`repo` directly and
`match $item.state` compares against the same wire spellings `transition` accepts. Every row carries
every field — an absent optional is `null`, never a missing key — so a sweep over a half-dispatched
board does not error on `$item.runner`. `query` additionally takes the reserved **`depends_on`**
filter (`satisfied` / `unsatisfied`), which makes "ready and unblocked" one call: an item is
unblocked exactly when every id in its `depends_on` is `done`; no dependencies is trivially
unblocked, and an absent dependency never resolves. `list`'s filter vocabulary is unchanged.
`comments` (C-236) is the read half of `comment` — the item's notes as a JSON array, oldest first.

Backends: `markdown` (durable, file-per-item) and `memory` (in-process). See
[`fleet-coordinator.md`](../../../docs/designs/fleet-coordinator.md).

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

## Surface ops (agent-authored panes)

Registered by `flux_tools::try_register_surface_ops`, and **surfaced by the presence of a
`SurfaceSink` at assembly time** (C-223) — not by a group and not by a signal. A host that installed a
pane channel registers the vocabulary once, for the life of the catalog; a host without one (headless
`flux run`, `flux-server`, an SDK embedding) never advertises these ops at all, and a call that
reaches a dispatch context with no sink fails with that reason rather than silently drawing nothing.

A pane is a *durable container* for status or results the user should keep seeing — not a place to put
the answer, which still belongs in the reply. The model proposes `slot` and `lifetime`; the surface
owns geometry, colour, ordering, bounds and the mark that says a region is agent-authored, and there
is no payload field that reaches a `Style` (C-220/C-222).

`data` is one object with exactly one key naming the shape: `rows` (`header`, `rows`), `kv` (`pairs`),
`log` (`lines`), `progress` (`label`, `done`, `total`), `tree` (`roots`) or `markdown` (`text`).
`lifetime` is `turn` or `session`; `project` is refused until a story builds the on-disk pane store.

| op | signature | risk | description |
|---|---|---|---|
| `pane.open` | `id, title, data[, slot, kind, lifetime]` | Low | Open a pane under `id` (your handle for later calls). `slot` defaults to `right`, `lifetime` to `session`; `kind` is derived from `data` and only checked if you also state it. Re-opening a live `id` replaces that pane in place rather than adding a second one; `host:*` ids belong to the surface's own panes and are refused |
| `pane.update` | `id, data` | Low | Replace an open pane's content — the whole payload, not a delta, and a payload of another shape re-renders the pane in that shape. An `id` that is not open (never opened, closed, or `turn`-scoped after the turn ended) is dropped by the surface |
| `pane.close` | `id` | Low | Close the pane opened under `id`. Closing an `id` that is not open is not an error and changes nothing; `turn`-scoped panes close themselves at the end of the turn, and no pane outlives the session |

All three declare no filesystem, process or network effect — a pane reaches none — and carry the
`human_visible` semantic effect. `permission_subjects` is the pane id, so a rule may scope a pane by
name (`pane.update:build`). They are `Conditional` rather than `Idempotent` on purpose: repeating a
pane command is safe, but `Idempotent` would let the op cache answer without the surface ever seeing
the repeat. Design: [`agent-authored-surface.md`](../../../docs/designs/agent-authored-surface.md).

## Typed user interaction (interactive surfaces only)

`user.ask` is registered only when the host attaches a `UserInteraction` responder. The stock
plain terminal and TUI attach one; stream-JSON, served/A2A, app and other headless assemblies do
not advertise the operation. A human answer is data, never an approval decision.

| op | signature | risk | description |
|---|---|---|---|
| `user.ask` | `prompt, schema` | Low | Present `prompt: {text, audio?}` through the attached host UI and wait for a value matching `schema`. Boolean, enum, unique enum-array and simple flat-object schemas map to native controls; other bounded schemas use validated JSON input. Returns `{status: "submitted", value, input_mode}` or `{status: "cancelled"}`. Audio is an opaque host asset reference and is accepted only when the host declares support |

The runtime bounds and validates the schema before presentation and validates the submitted value
again before it becomes a tool result. Remote references, secret-shaped fields, password/write-only
fields and secret-bearing responses fail closed. Design:
[`user-interaction.md`](../../../docs/designs/user-interaction.md).

## Eval & self-improvement ops (group `eval`)

Registered by `flux_eval::try_register_eval_ops`, wired into the production catalog by
`flux-cli/src/execution.rs`. They are evidence-gated on an `eval` signal (a `.flux/evals/` directory),
so a workspace without one never sees them — **gated off is still public surface**, which is exactly
why they belong here. They drive `examples/improve.flux` / `examples/improve-tbench.flux`; the
self-improvement loop's own contract is in
[`docs/self-improvement/DESIGN.md`](../../../docs/self-improvement/DESIGN.md).

### Measure

| op | signature | risk | description |
|---|---|---|---|
| `eval_run` | `adapter[, limit, model, tasks]` | Medium | Run a benchmark suite against the flux binary → `{adapter, pass_rate, scalar, total, passed, mean_*, cases}`. `adapter` is `mock` (offline fixture), `synthetic` (real-model coding riddles), `terminal-bench` (the real Docker benchmark), or `multi` (several behind one combined score) |
| `eval_scalar` | `report` | Low | The report's score scalar as a plain string (e.g. `"667"`) |
| `eval_report_md` | `report` | Low | Render a report as categorized Markdown: headline score, per-task table, mined pain-points |
| `eval_sessions` | `report` | Low | The session references `[{id, db, task_id}]` a report ran through |
| `eval_adopt` | `report` | Low | Return a report unchanged — the seam that re-binds the baseline after a candidate is adopted |
| `score_compare` | `candidate, baseline` | Low | `"true"` iff the candidate report is strictly better than the baseline |
| `score_compare_multi` | `candidate, baseline` | Low | `"true"` iff better overall **and** no member benchmark regressed (pass-rate *and* check-rate ≥ baseline) |
| `grade` | `criterion` | Medium | Evaluate one verifiable pass/fail `Criterion` (`command`/`file_content`/`all`) against the workspace → `"true"`/`"false"` (also listed under the agent-loop ops, which reuse the same grader) |

`score_compare_multi` is the keep-gate for combined evals: one blended score can hide a regression, so
a gain on one benchmark must not be allowed to mask a loss on another.

### Mine and rank

| op | signature | risk | description |
|---|---|---|---|
| `sessions_digest` | `sessions` | Low | Render each session's `RunEvent` trace into a compact transcript for review |
| `painpoints_collect` | `sessions` | Low | Mine pain-points — tool errors, retry loops, missing tools, churn — from session references `[{id, db}]` |
| `improvements_aggregate` | `mined, reviewed` | Low | Cluster deterministic pain-points (`mined`) and LLM review findings (`reviewed`) into ranked improvement candidates |
| `candidates_advance` | `candidates` | Low | Drop the consumed first candidate and return the rest |
| `candidates_empty` | `candidates` | Low | `"true"` iff the candidate list is empty |
| `improve_log` | `record` | Medium | Append a timestamped round record to `.flux/eval/improve-log.jsonl` (the audit trail) |

### Act, and keep the measurement honest

| op | signature | risk | description |
|---|---|---|---|
| `change_implement` | `tasks[, limit]` | Medium | Implement each derived task by spawning a `worker` sub-agent → a per-task summary. `limit` caps the round (`0` = all) |
| `gate_check` | `[build, test, clippy, fmt, timeout_secs]` | Medium | Run the dev gate (`cargo build`/`test`/`clippy`/`fmt --check`) → `"true"` (all green) or `"false"`; each step is individually toggleable and `timeout_secs` bounds each one |
| `git_snapshot` | | Low | Capture `HEAD` as a round snapshot; **errors if the working tree is dirty**, so a round can always be undone |
| `guard_protected` | `snapshot` | Medium | Restore the grader/suite/loop/CI paths to the round snapshot after the worker runs → `{tampered, restored}`; **refuses a snapshot it cannot place on this checkout's line** |
| `git_reset` | `snapshot` | Destructive | Hard-reset the working tree to a `git_snapshot`, discarding the round's changes → `{reset_to, discarded}`; **refuses a snapshot it cannot verify** |
| `git_tag` | `name[, message]` | Medium | Tag the current commit (`name` is a prefix — the short `HEAD` sha is appended for uniqueness; annotated when `message` is given) |

`guard_protected` is the anti-cheat step, and it is the reason a round is measurable at all: the worker
can edit the harness, so the grader, the suite, the loop preset and the CI definition are restored to
the snapshot *before* the candidate is scored. Tampering is reported rather than swallowed.

`git_reset` is the only op in this family at the `Destructive` tier, and it is approval-gated
accordingly. Note the name: the **builtin** `git_revert` appends an inverse commit and never touches
the working tree, while this op discards it. They are different operations with different blast
radii — C-238 renamed this one out of the collision.

Because `git_reset` is a *blanket* restore — `reset --hard` plus an unscoped `git clean -fd`, which
deletes untracked files outright — C-278 gave it a precondition. What it guarantees is bounded and
worth stating exactly: **a reset can only rewind within this checkout's own line of history.** Two
things are checked, and they are not equally strong. The snapshot must carry `clean: true`, which
only `git_snapshot` sets and only after finding the tree clean — but nothing verifies the payload
came from `git_snapshot`, so a flow writing `git_reset({"head": h, "clean": true})` by hand is
licensed anyway; that check catches the caller who forgot to snapshot, not one that lies. The second
check, that `head` is an ancestor of the current `HEAD`, asks git rather than the payload and is the
one no caller can talk its way past. A snapshot taken on a divergent line is refused with the
working tree listed and untouched. Neither check looks at *recency*, so a snapshot reused from an
earlier round is still honoured and the commits since are rewound. What a licensed reset destroyed
comes back in `discarded` — though that is built from `git status --porcelain`, so it reports
working-tree losses only, never rewound commits.

`guard_protected` answers the two halves separately. On *which paths* it may touch it needs no
precondition and states an exemption instead: its `checkout` and `clean` argv always end in `--` and
an explicit pathspec list filtered through the `PROTECTED` set, so it cannot reach a path outside it
however dirty the tree is. Requiring a clean tree would also be incoherent — the op runs *after* the
worker has deliberately dirtied one, so `clean: true` is deliberately **not** demanded here. On
*which commit those paths are restored from* it takes the same ancestry check as `git_reset`, added
by C-281: a `head` that is not an ancestor of the current `HEAD` is refused with the working tree
listed and nothing touched. A bogus sha always failed safe by accident; a valid but divergent one
used to reset the anti-cheat baseline to an unrelated line of history without a word.

## Release ops (`examples/release.flux`)

Registered by `flux_eval::register_eval_ops` alongside the self-improvement pack above — top-level-only,
repo-mutating orchestration ops, never a sub-agent's. They exist as five narrow ops rather than as
`proc.run` calls because a release flow needs exactly two programs and three writable files, and fixed
argv plus a fixed path set is authority that cannot be mistyped.

| op | signature | description |
|---|---|---|
| `release_plan` | | The last `v*` tag, the commit subjects + diffstat since it, and the **host's** bump and next version |
| `release_verify_versions` | | `scripts/check-crate-versions.sh` (fixed argv); errors with the offending protocol-line crate named |
| `release_parse_notes` | `text` | Strictly parse the scribe's textual JSON contract into typed release-note fields; normalizes one canonical `json` fence, rejects surrounding prose and schema drift; pure, with no external authority |
| `changelog_insert` | `file, body[, section][, apply]` | Insert markdown under a changelog's `## [<section>]` heading, deterministically and idempotently; `apply` defaults to false (preview) |
| `release_cut` | `bump[, apply]` | `scripts/cut-release.sh <bump>` (fixed argv), stopping at the **local** annotated tag; never pushes, never publishes; `apply` defaults to false |

**The model writes prose, the host decides the version.** `release_plan` derives the bump from
conventional-commit titles (`!` ⇒ breaking ⇒ minor while `0.y`), so no version is ever read back out
of a model reply — crates.io is yank-only and a wrong version cannot be withdrawn. A model may return
a `bump_opinion`; a disagreement is surfaced as a `release.bump_disagreement` observation and changes
nothing. `task()` returns text, so `release_parse_notes` validates the exact object shape before the
flow reads fields. `changelog_insert` addresses only `CHANGELOG.md`, `WHATS-NEW.md` and
`website/docs/whats-new.md`, resolved through the canonicalizing IO boundary first, so model text is an
*input* to the file rather than its contents.

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
| `ai_segment` | `goal, tools, max_rounds[, current_turn, max_tokens, max_history_bytes]` | Run a bounded adaptive segment inside a deterministic flow. The required authored `max_rounds` is the segment's exact provider-call ceiling (it is not clamped by the normal agent default). The authored `tools` list is a hard live capability ceiling; reads gather evidence, effects use the same batch path, and the result is returned as `{result, state[, decision]}`. Retained history above `max_history_bytes` (512 KiB by default) sheds the oldest tool-result payloads into digest receipts rather than failing the turn. Provider usage folds into the enclosing turn. |
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

`detect_intent` and `explore` enforce their tagged-object output contract at the host adapter before
authored control flow reads `.kind`. A scalar or object without a string `kind` fails the stage,
names the producer and returned type, and retains only a bounded executor-redacted excerpt. The loop
does not guess a default intent or continue from malformed state.

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
