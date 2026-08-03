---
title: Operations
description: The registered operations a Flux-Lang call can target — core tools, toolchains, git, cognition ops, and app orchestration ops.
---

# Operations

Operations are the callable boundary between Flux-Lang and the host runtime. A `call` node names an
operation; the host decides whether that operation exists, how arguments are validated, and what
approval or policy checks apply.

The engine advertises a catalog built from the live tool registry. [Plugins](../plugins/authoring.md)
project additional operations into the same catalog.

The catalog is also **evidence-gated**: tool groups surface when the workspace shows their
signal (the `cargo_*` ops appear in Rust workspaces, `go_*` alongside Go modules), and the
generic `bash` op is opt-in — plans are steered toward dedicated, accurately-gated ops.

Arguments below are named; pass them directly (`read(path: "…", limit: 100)`), or use the bare
form for a sole required parameter (`read("README.md")`). Optional arguments are in
`[brackets]`. Ops marked **approval** may pause for user approval depending on the active
policy.

## Files, search, and web

| op | arguments | risk | description |
|---|---|---|---|
| `read` | `path[, limit, offset]` | low | Read a file (line-numbered), a list of files, or a glob pattern |
| `read_many` | `paths` | low | Read several files at once, sections headed per path |
| `grep` | `pattern[, glob, literal, max_results, path]` | low | Regex search; `literal: true` for plain substrings |
| `glob` | `pattern[, path]` | low | List files matching a glob pattern |
| `file_stat` | `path` | low | Size, line count, mtime |
| `path_exists` | `path` | low | `"true"`/`"false"` — branch on file presence with `when`/`unless` |
| `write` | `path, content` | medium, approval | Create or overwrite a file |
| `edit` | `path, old_string, new_string[, replace_all]` | medium, approval | Replace a string in a file (exact-match first, then progressively looser anchoring) |
| `patch` | `path, edits` | medium, approval | Several line-anchored edits in one call |
| `append` | `path, content` | low, approval | Append to a file, creating it if absent |
| `sources` | | low | Enumerate the [datasource](../agent/datasources.md)'s sources: entity types + record count per source |
| `search` | `query[, source, entity, harness, limit]` | low | Keyword search over the indexed [datasource](../agent/datasources.md); `harness` restricts to one local coding harness's history, and is advertised only where the host opted that in |
| `get` | `source, entity, id` | low | Fetch one datasource record in full by its address |
| `list` | `source[, entity, offset, limit]` | low | Enumerate a datasource source's records, paged |
| `relation` | `source, entity, id[, rel]` | low | Follow a datasource record's typed links |
| `batch_get` | `source, entity, ids` | low | Fetch several datasource records in one call |
| `web.fetch` | `url[, raw]` | medium | Read a URL as a document: HTML becomes condensed Markdown, PDFs become extracted text; `raw` preserves the body |
| `web.crawl` | `url[, max_pages, max_depth, max_total_bytes]` | medium | Crawl a small site or section: from a seed, follow same-host links breadth-first (bounded by `max_pages`/`max_depth`, and optionally a total-content `max_total_bytes` budget that stops the crawl early), returning each page as condensed Markdown |
| `html_to_markdown` | `html` | low | Pure conversion of an HTML string to condensed Markdown; no network access |
| `http.request` | `url[, method, query, headers, body, timeout]` | medium, approval | Arbitrary HTTP request returning the record `{status, headers, body}` — select a field with `resp.body.data.id`. `body` is the parsed JSON when the response is a JSON object or array, and the raw capped text otherwise; non-2xx remains a result. Pass parameters as the `query` record — each value is percent-encoded, so a value carrying `&` or `=` cannot add a parameter |
| `browser.open` | `[url]` | medium, approval | Start a headless-Chromium session and return a non-visual page digest |
| `browser.goto` | `session, url` | medium, approval | Navigate an existing browser session and return a delta |
| `browser.snapshot` | `session[, view]` | low | Re-observe a session (`full`, `actions`, or `content`) |
| `browser.act` | `session, action[, ref, value, full]` | medium, approval | Click, type, fill, select, press, scroll, navigate, or go back using digest refs |
| `browser.close` | `session` | low | Close a browser session and its Chromium child |
| `web.search` | `query|queries[, max_results, providers]` | low | First-party `websearch` plugin alias: Tavily with host-managed auth, otherwise DuckDuckGo; no credential field |
| `sqlite_query` | `db, sql[, limit]` | low | Read-only SQLite query (`limit` caps rows, default 200) |
| `now` / `cwd` / `home_dir` / `sys_info` | | low | Clock, workspace/home paths, and host metadata — no shell needed |

All native web operations share the `[private_net] web` scope. Public destinations are allowed by
default; private/internal destinations require an explicit grant. Browser operations register in
every host but are advertised only when a Chromium binary is discoverable. `web.search` is the
model-facing alias projected by the first-party `websearch` plugin; its destinations are additionally
bounded by that plugin's manifest and its Tavily credential is injected host-side.

## Processes and toolchains

| op | arguments | risk | description |
|---|---|---|---|
| `bash` | `command[, timeout_secs]` | high, approval | Run a shell command — **opt-in**, off by default |
| `proc.run` | `program[, args, timeout_secs]` | high, approval | One argv-only process, no shell, cleared env |
| `task` | `role, task` | medium, approval | Delegate to a sub-agent role |
| `cargo_check` / `cargo_build` / `cargo_test` / `cargo_clippy` / `cargo_fmt` | `[package, args, …]` | medium, approval | The Rust toolchain (Rust workspaces) |
| `go_build` / `go_test` / `go_vet` | `[package, args]` | medium, approval | The Go toolchain (Go workspaces) |
| `python_run` / `pytest` | `[script, module, path, args]` | medium, approval | Python scripts and tests |
| `npm` / `node_run` | `args` / `script[, args]` | medium, approval | Node tooling |
| `make` | `[target, args]` | medium, approval | Run make (surfaces on a Makefile) |

## Git

| op | arguments | risk | description |
|---|---|---|---|
| `git_status` | | low | Working tree status |
| `git_diff` | `[path, staged]` | low | Unstaged (or staged) diff |
| `git_log` | `[limit]` | low | Recent commits |
| `git_hunks` | `path[, context]` | low | List the unstaged diff as individually addressable hunks |
| `git_stage_hunks` | `path, hunks[, context]` | medium | Stage selected hunks by id, leaving the rest of the file unstaged |
| `git_stage` / `git_unstage` | `paths` | medium / low | Stage or unstage files |
| `git_commit` | `message[, body]` | medium | Create a commit |
| `git_push` | `[branch, remote]` | medium | Push to a remote |
| `git_checkout` | `branch[, create]` | medium | Switch or create a branch |
| `git_branch` | `name[, delete]` | medium | Create a branch without switching to it, or safe-delete one (`-d` refuses unmerged work and the checked-out branch) |
| `git_merge` | `branch[, no_ff]` | high | Merge a ref into the current branch (`no_ff` forces a merge commit); a conflict is a recoverable error naming the conflicting files — the merge is aborted and the tree restored, never left half-merged. Refuses outright if a merge is already in progress, and aborts nothing in that case: the in-flight resolution may be uncommitted work |
| `git_revert` | `commit[, mainline]` | high | Revert a commit by appending its inverse (`mainline`, usually 1, for a merge) — a new commit undoes the target, never a reset; requires a clean tree, and a conflicted revert is aborted and left clean, naming the conflicting files |
| `git_worktree_enter` | | high | Move this agent context into an isolated temporary git worktree (requires a clean `main`; creates a generated `flux/worktree/*` branch) |
| `git_worktree_leave` | | high | Merge the worktree's committed work back into `main` (`--no-ff`, guarded by an aborted trial merge), remove the worktree and branch, restore the original root |

None of these ops rewrites history. `git_revert` undoes a commit by adding a new one on top, so the
commit it reverts stays in the log and nothing already pushed is invalidated.

:::caution `git_revert` changed meaning — the old one is now `git_reset`

`git_revert` used to name the [improvement loop](#improvement-loop)'s snapshot op, which hard-resets
the working tree and **discards** uncommitted changes. That op is now called **`git_reset`**, and
`git_revert` is the true revert described above. There is no alias: a flow that calls
`git_revert(snapshot)` must be changed to `git_reset(snapshot)`.

The rename matters because the call still looks valid. `git_revert(snapshot)` now asks git to append
the inverse of that commit instead of resetting to it — a different outcome, and one that errors on a
dirty tree rather than clearing it. If you have a flow that restored a snapshot, rename the call.

:::

## Cognition ops

The cognition pack splits into **pure** data-shaping ops — deterministic, no IO, never pause
for approval — and **model-backed** ops that make one structured model call each. The
model-backed ops carry a network effect and are advertised only when the host registers a
provider for them.

Pure:

| op | arguments | description |
|---|---|---|
| `need` | `ask, require[, done_when]` | Build a `Need` artifact — an explicit statement of missing info |
| `gaps` | `claims, need` | Report a `Need`'s still-unmet required fields |
| `compare` | `a, b` | `{added, removed, common}` over two arrays |
| `dedupe` | `items[, by]` | Remove duplicates, first-seen order; `by` accepts dotted paths |
| `sort` | `items[, by, order]` | Stable sort by a field or dotted path, `asc`/`desc` |
| `top` | `items, n` | First `n` items |
| `skip` | `items, n` | Drop the first `n` items |
| `merge` | `lists` | Concatenate an array of arrays |
| `map` | `items, path|expr[, vars]` | Project each item by dotted path or an expression with `it` bound |
| `filter` | `items[, where, vars, by, equals]` | Keep items by expression predicate or dotted field/equality |
| `flatten` | `items[, depth]` | Flatten nested arrays up to `depth` levels |
| `join` | `items[, sep]` | Join stringified items into plain text |
| `split` | `s[, sep, trim]` | Split text into a JSON array of strings |
| `sum` | `items[, path]` | Sum numbers, optionally plucked from a dotted path |
| `count_by` | `items, path` | Count items by dotted path, sorted by count desc then key |
| `group_by` | `items, path` | Group items by dotted path in first-seen key order |
| `any` | `items[, where, vars]` | `"true"` when any item is truthy or matches an expression |
| `all` | `items[, where, vars]` | `"true"` when all items match; empty lists are vacuously true |
| `has` | `items, value` | `"true"` when the array contains `value` by JSON equality |
| `pick` | `items, keys` | Keep only listed object keys; accepts one object or an array of objects |
| `omit` | `items, keys` | Remove listed object keys; accepts one object or an array of objects |
| `merge_obj` | `objects` | Shallow-merge objects left to right; later keys win |
| `coalesce` | `values[, default]` | First value that is neither `null` nor `""`; otherwise `default` or `null` |
| `keys` / `values` | `item` | Object keys or values in deterministic order |
| `len` / `first` / `last` | `items` | Count, first item, last item |
| `regex_match` | `s, pattern` | Returns `"true"` or `"false"` if `s` matches the regex pattern (ReDoS-free) |
| `regex_extract` | `s, pattern[, group, all]` | Extract text matching pattern; returns first match or null, or all matches with `all: true` |
| `cite` | `claims` | A markdown citation list, one line per claim |

**Ops that select an existing string hand back the string itself.** `regex_extract` (single match),
`first`, `last` and `coalesce` bind the bare text, with no surrounding quote characters — so an
extracted URL can be passed straight to an op that fetches one. Anything they select that is *not* a
string (an object, an array, a number, a boolean, `null`) comes back as JSON, which the runtime reads
back as structured data. Ops that build a new value are unaffected: `split` and `keys` still return
arrays, and `regex_extract` with `all: true` still returns an array of matches.

**Examples:**

```flux
authors = map(items: issues, path: "author.username")
open = filter(items: issues, vars: { "min": 2 }, where: "it.state == 'opened' && it.upvotes > min")
all_pages = flatten(pages)
rest = skip(items: candidates, n: 1)
report = join(items: lines, sep: " | ")
hosts = split(s: raw_hosts, sep: ",", trim: true)
total = sum(items: invoices, path: "amount")
by_status = count_by(items: issues, path: "state")
grouped = group_by(items: issues, path: "author.username")
has_bug = has(items: labels, value: "bug")
all_green = all(items: checks, where: "it.status == 'ok'")
slim_issues = pick(items: issues, keys: ["iid", "title", "state", "web_url"])
public_issue = omit(items: issue, keys: ["author_email", "raw_payload"])
merged = merge_obj([defaults, overrides])
assignee = coalesce(default: "unassigned", values: [issue.assignee.username?, issue.author.username?])
field_names = keys(issue)
field_values = values(issue)
has_error = regex_match(pattern: "ERROR", s: log_line)
when has_error
  alert(msg: "Error detected")
version = regex_extract(group: 1, pattern: "v(\\d+\\.\\d+\\.\\d+)", s: "flux-cli v1.2.3")
emails = regex_extract(all: true, pattern: "\\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\\.[A-Za-z]{2,}\\b", s: body_text)
```

Model-backed:

| op | arguments | description |
|---|---|---|
| `ai.extract` | `from[, ask, schema]` | Extract typed items (e.g. `Claim`s) from free text |
| `ai.rank` | `items[, by]` | Reorder items by a natural-language criterion |
| `ai.judge` | `claim[, evidence]` | Adjudicate a claim into a `Verdict` |
| `ai.reason` | `ask[, ctx]` | Free-form reasoning over a [context pack](./context-packs.md) |
| `synth` | `claims[, format, cite]` | Synthesize a cited `Answer` from claims |
| `ai.rewrite` | `text[, style]` | Rewrite text in a requested style |

These produce and consume the [prelude artifact types](./types-and-effects.md) — `Claim`,
`Evidence`, `Verdict`, `Answer`, and friends — so multi-step reasoning pipelines stay typed.

## Second opinion

| op | arguments | risk | description |
|---|---|---|---|
| `consult` | `question[, context, model]` | low | Ask a DIFFERENT model for a second opinion — pure advice, no tools |

`consult` is the one op that deliberately calls a model **other than** the agent's own — typically
a stronger or differently-biased one — for a hard sub-question. It takes a question plus
caller-supplied context and an optional `provider/model` override, makes exactly one model call,
and returns the answer as text. It carries no filesystem, process, or network authority beyond
that single call, so it adds no new authority to the safety envelope; the reply enters context as
untrusted content. Only surfaced when an operator has configured a default target
(`[consult] model` in `.flux/config.toml`) — see [configuration](../reference/config.md) — so an
unconfigured workspace never sees a churn-prone catalog entry. A per-turn call cap
(`[consult] max_calls`) keeps it a cheap escape valve, not a council of models.

## App orchestration ops

Registered **only** by the `flux app run` host for [multi-agent programs](../agent/programs.md)
— journeys use them to drive the event bus and channels:

| op | arguments | description |
|---|---|---|
| `emit` | `event[, payload]` | Publish an event to the bus (fires matching triggers) |
| `send` | `channel, message` | Send a message to a named channel |
| `ask` | `channel, message` | Send and return a correlation id |
| `spawn` | `run[, input]` | Run a named journey to completion and return its result |

## Fleet ops

`task` delegates to a **local** sub-agent and waits for it. The `fleet.*` ops are the half `task`
cannot express — hand work to a **remote** flux worker (a `flux serve` instance reachable over A2A)
without waiting, then poll or stop it. A coordinator can therefore keep many workers in flight and
reconcile them later.

| op | arguments | description |
|---|---|---|
| `fleet.dispatch` | `worker, task[, role, context_id]` | Send a task to a remote worker and return its task id without waiting. A worker that replies synchronously returns a null task id plus its answer, instead of an id that would be polled forever |
| `fleet.status` | `worker, task_id` | Read a dispatched task's current state, whether it is terminal, and its final text |
| `fleet.cancel` | `worker, task_id` | Stop a dispatched task. An already-finished task reports that it was not cancelable |
| `fleet.isolate` | `item` | Create branch `impl/<item>` in its own git worktree and return the checkout path — a per-item isolated workspace for one local worker. Unlike a worktree session it does not move the caller's own working root, so one call per item in a wave is legal. Requires a clean checkout and a free branch name; removing the worktree afterwards is the caller's job |
| `fleet.start` | `item[, worktree, context_id, model]` | Start a flux worker for one board item and return the endpoint to dispatch to. `worktree` confines it to an isolated checkout; the returned context id resumes the same worker session later. Reaching the returned endpoint needs `--allow-private-net`, since it is a loopback address |
| `fleet.worker_status` | `worker_id` | Report whether a worker is starting, live, or dead — with its exit code and the tail of its own output when it died. The worker's liveness, not a task's state |
| `fleet.stop` | `worker_id` | Stop a worker started by `fleet.start`. An already-exited worker succeeds; an unknown worker id is an error |

The `worker` address is an argument, not configuration, so it is model-reachable and gated as such.
Every call resolves the endpoint through the same egress guard as `web.fetch` before any request,
and the approval subject is the worker's **origin** — never a wildcard. An address flux cannot parse
reports no subject at all, which forces an approval prompt rather than matching a broad grant. These
ops carry no standing private-network grant: they are not part of the `web` egress scope, so
reaching a worker on a private address needs the explicit `--allow-private-net` override. A worker
behind `flux serve`'s bearer token is not reachable yet.

`fleet.status` is never served from the operation cache — observing the change since the last poll is
the point of a status call.

**Work board operations are not in this catalog**, because they do not exist until a program asks for
them. A `datasource` with a `board:` kind generates the board operations named after *that
declaration*, so a board declared as `board` yields `board.list` … `board.record_evidence` while one
declared as `queue` yields `queue.list` … `queue.record_evidence`. Nothing is callable without the
declaration.
They are documented with the declaration that creates them, in
[Work boards and the fleet](../agent/fleet.md).

## Endpoints

Registered as one evidence-gated group when a kubeconfig is present or the persisted endpoint store
was non-empty at session startup. See [Endpoints](../agent/endpoints.md).

| op | arguments | description |
|---|---|---|
| `endpoint.discover` | `product[, query, limit]` | Ask installed discovery providers for ranked weak endpoint references |
| `endpoint.list` | | List endpoint records known in the session registry |
| `endpoint.info` | `id` | Inspect one endpoint record and its credential location |
| `endpoint.select` | `id` | Return one model-safe `EndpointRef` for reuse in another operation |
| `endpoint.import` | `id` | Persist a known record to `~/.flux/endpoints.toml` (approval-gated local write) |

## Agent-invoked commands and skills

Registered as one evidence-gated group, surfaced only when the session discovers at least one
command file or skill explicitly marked `agent-triggerable: true` in its own frontmatter (default
`false` — human-only stays the default). See [Claude compatibility](../agent/claude-compat.md).

| op | arguments | description |
|---|---|---|
| `command.invoke` | `kind, name[, arguments]` | Invoke a discovered command file (`kind: "command"`) or skill (`kind: "skill"`) by name — only when it is policy-permitted, discovered in this session, AND explicitly `agent-triggerable`. A command expands `$ARGUMENTS`/`$1..$9` and returns the substituted body as prompt text (no nested execution); a skill returns its body. Any missing gate is a clean refusal, never a partial run |

## Panes on the terminal

Surfaced only when the host running the session installed a pane channel — the interactive TUI does;
a headless `flux run`, the HTTP server and SDK embeddings do not, and their models never see these
ops. The decision is taken once, when the session's catalog is assembled, so the advertised tool set
never changes mid-session.

A pane is a durable container for status or results the user should keep in view (a build's progress,
a checklist, a table of findings) — not a place to put the answer, which stays in the reply. The
agent proposes where a pane sits and how long it lives; the surface owns geometry, colour, bounds and
the mark that identifies a region as agent-authored, and the payload carries no styling field at all.

`data` is one object with exactly one key naming its shape: `rows` (`header`, `rows`), `kv`
(`pairs`), `log` (`lines`), `progress` (`label`, `done`, `total`), `tree` (`roots`) or `markdown`
(`text`). `slot` is `left`/`right`/`bottom`/`overlay` (default `right`) and `lifetime` is
`turn`/`session` (default `session`).

| op | arguments | risk | description |
|---|---|---|---|
| `pane.open` | `id, title, data[, slot, kind, lifetime]` | Low | Open a pane under `id`, the handle later calls address. Re-opening a live `id` replaces that pane rather than adding a second one; ids beginning `host:` belong to the surface's own panes and are refused |
| `pane.update` | `id, data` | Low | Replace an open pane's content — the whole payload each time. An `id` that is not open (closed, or `turn`-scoped after the turn ended) is dropped by the surface |
| `pane.close` | `id` | Low | Close the pane opened under `id`; closing one that is not open is not an error. `turn`-scoped panes close at the end of the turn, and no pane outlives the session |

Permission rules may scope a pane by name (`pane.update:build`), and pane content passes the same
redactor as every other tool result before it reaches the screen.

## Typed user interaction

The local terminal and TUI can expose one schema-driven question operation. Headless, served,
stream-JSON and app surfaces omit it, so an agent never waits on a human where no question UI exists.

| op | arguments | risk | description |
|---|---|---|---|
| `user.ask` | `prompt, schema` | Low | Ask through the attached UI and wait for a schema-valid value. Boolean, single-choice, multi-choice and simple form schemas use native controls; unusual schemas fall back to validated JSON. Returns an explicit `submitted` value or `cancelled` status |

Questions and approvals are separate: answering a question cannot authorize an operation. Schemas
and answers are size-bounded, validated on both sides of the UI, and refused if they request or
contain secret material. Audio prompts use opaque host-owned asset references and work only in an
SDK host that declares audio support; the stock terminal surfaces are text-only.

## Model-invoked skills

Opt-in Claude-style progressive skill disclosure (`--skills-model-invoked` / `[skills]
model_invoked`): every discovered skill's name+description is surfaced compactly, and the model
loads a body on demand instead of every skill's full text being injected up front. Off by default —
manual `--skill` activation stays the (measured cheaper) default path. Advertised only when the
opt-in is on and at least one loadable skill was discovered; a skill declaring
`disable-model-invocation: true` never enters this catalog. See [Model-invoked skills
(opt-in)](../agent/skills-and-roles.md#model-invoked-skills-opt-in).

| op | arguments | description |
|---|---|---|
| `skill.load` | `name` | Pull one skill's full body into context by exact `name`. Idempotent and persistent: once loaded, the skill behaves like an explicitly `--skill`-activated one for the rest of the session |

## Flows

Discover and run reusable flows and composite ops stored under `.flux/flows` (project) and
`~/.flux/flows` (global) — see [Where flows live](./tooling.md#where-flows-live):

| op | arguments | description |
|---|---|---|
| `flow_list` | | List the flows and composite ops in the flows home, each with its description and params |
| `flow_run` | `name\|path[, inputs]` | Run exactly one stored-flow `name` or workspace-relative `.flux` `path`; path source is reread for every call, an `inputs` object is seeded as `$key` binds, and the flow is checked against the current operation catalog before guarded execution. Returns the resolved path, flow name, and seeded input keys as a route receipt |
| `flow_render` | `source\|name[, view]` | Render Flux-Lang as a syntax-highlighted SVG — inline `source` or a stored flow `name`; `view: "source"` (default) renders the highlighted source, `view: "tree"` the execution-path plan tree. Returns SVG markup inline for surfaces that can't highlight `.flux` (READMEs, Slack, docs, chat) |
| `op.register` | `source, scope[, replace, expose]` | Validate and register one composite op for the turn, session, project, or global scope |

## Evidence and strict-review helpers

These deterministic helpers are registered with the built-ins. The default loop uses the evidence
family internally; strict-review flows use the review family directly.

| op | arguments | description |
|---|---|---|
| `observe` | `kind[, data]` | Append a structured observation to the current run's evidence log |
| `evidence` | `[kind]` | Read all observations, or only one kind |
| `metrics` | | Summarize tool calls, errors, and iterations from the evidence log |
| `review.normalize` | `findings` | Normalize raw reviewer output and quarantine malformed entries as gaps |
| `review.aggregate` | `findings[, files, reviewers]` | Deduplicate, rank, and summarize findings into a stable review report |

## Scheduled wake-ups

| op | arguments | description |
|---|---|---|
| `schedule_wakeup` | `in_secs, prompt` | Register a future wake-up on **this** session: after `in_secs`, the session resumes with `prompt` |

`Medium` risk, `LocalSystem` effect. Off by default and absent from the catalog entirely until
`[wakeup] enabled = true` — see the [configuration reference](../reference/config.md#scheduled-wake-ups-wakeup)
for the horizon and pending-count bounds, and `flux wakeups list | cancel` in the
[CLI reference](../agent/cli.md) to inspect and revoke them. Enabling the table does not grant the
op: registration still needs approval-gated `host.write` authority.

## Agent-loop stages

These belong to the `reflect` group, which is **never surfaced into a model-facing catalog** — the
model cannot call them. They exist so the agent turn loop can be *authored* in Flux-Lang rather than
hard-coded, and you only write them when supplying your own loop via `[agent] loop`.

| op | arguments | description |
|---|---|---|
| `detect_intent` | turn context | Detect the turn's intent and resolve capability signals into a durable `IntentSet` |
| `explore` | exploration state | Continue evidence gathering and native-schema action proposal; may return more work |
| `approve_batch` | `batch` | Request aggregate approval for one immutable `ActionBatch`; returns a session-bound one-shot receipt |
| `execute_batch` | `batch, receipt` | Consume a matching receipt and dispatch every operation through authorization and guarded IO |
| `ai_segment` | scope, exit condition | Hand a bounded run of model turns to the loop under a capability scope and an explicit exit condition |
| `present_results` | stage artifact | Render a terminal adaptive-stage artifact into channel-neutral answer text |

The approve/execute split is the safety envelope made explicit: `approve_batch` produces a receipt
bound to one immutable batch, and `execute_batch` refuses anything the receipt does not match. See
[The agent loop](../agent/agent-loop.md) and [Durability](./durability.md) for `ai_segment`'s role
in bounded adaptive work.

## Improvement loop

The ops the self-improvement flows orchestrate. They are registered in every session, so a flow can
call them directly.

:::note Status
The Improvement pillar is **de-prioritized and on hold** (since 2026-07-06). The machinery below is
real and runnable; the headline claim — a repeatable, grader-confirmed gain — is not yet proven. Use
these ops to measure and audit, not on the assumption that the loop reliably improves the harness.
See [Improvement](../agent/improvement.md).
:::

**Running and scoring evals**

| op | arguments | description |
|---|---|---|
| `eval_run` | `adapter[, limit, model, tasks]` | Run a benchmark suite against the flux binary; returns `{adapter, pass_rate, scalar, total, …}` |
| `eval_scalar` | `report` | The report's score scalar as a plain string |
| `eval_report_md` | `report` | Render a report as categorized Markdown (headline score, per-task table) |
| `eval_sessions` | `report` | Extract session references `[{id, db, task_id}]` from a report |
| `eval_adopt` | `report` | Return a report unchanged — used to re-bind the baseline after adopting a candidate |
| `score_compare` | `candidate, baseline` | `"true"` iff the candidate report is strictly better |
| `score_compare_multi` | `candidate, baseline` | `"true"` iff better overall **and** no member benchmark regressed |
| `grade` | `criterion` | Evaluate a pass/fail criterion against the current workspace |

`score_compare_multi` exists because a single combined score can hide a regression: it requires
every member benchmark's pass rate and check rate to be at least the baseline's.

**Mining and ranking improvements**

| op | arguments | description |
|---|---|---|
| `sessions_digest` | `sessions` | Render each session's run trace into a compact transcript for review |
| `painpoints_collect` | `sessions` | Mine pain-points — tool errors, retry loops, missing tools, churn — from session references |
| `improvements_aggregate` | `mined, reviewed` | Cluster mined pain-points (`mined`) and review findings (`reviewed`) into ranked candidates |
| `candidates_advance` | `candidates` | Drop the consumed candidate and return the rest |
| `candidates_empty` | `candidates` | `"true"` iff the candidate list is empty |
| `improve_log` | `record` | Append a timestamped round record to `.flux/eval/improve-log.jsonl` |

**Applying a round, under guard**

| op | arguments | description |
|---|---|---|
| `change_implement` | `tasks[, limit]` | Implement each derived task by spawning a `worker` sub-agent; returns a per-task summary |
| `gate_check` | `[build, test, clippy, fmt, timeout_secs]` | Run the dev gate (build/test/clippy/fmt) and return `"true"` or `"false"`; each step is individually toggleable |
| `guard_protected` | `snapshot` | Restore grader/suite/loop/CI paths to the round snapshot after the worker runs |
| `git_snapshot` | | Capture `HEAD` for later revert; errors if the working tree is dirty |
| `git_reset` | `snapshot` | **Destructive** — hard-reset the working tree to a snapshot, discarding the round's changes; refuses a snapshot it cannot verify |
| `git_tag` | `name[, message]` | Tag the current commit (`name` is a prefix — the short `HEAD` sha is appended; annotated when `message` is given) |

`guard_protected` is the anti-cheat step: it restores the grader, the suite, the loop, and the CI
config after each worker run, so a round cannot raise its own score by editing what measures it.
`git_reset` carries the `Destructive` risk tier and is approval-gated accordingly. It is a *blanket*
restore — `reset --hard` plus an unscoped `git clean -fd`, which deletes untracked files outright —
so it checks a precondition first, guaranteeing that **a reset can only rewind within this
checkout's own line of history**. The snapshot must carry `clean: true` (set by `git_snapshot`, and
only after it found the tree clean) and its `head` must be an ancestor of the current `HEAD`; a
snapshot from a divergent line is refused with the working tree listed and left untouched. Only the
second check is unforgeable — nothing verifies that a `clean: true` payload really came from
`git_snapshot` — so treat the guarantee as "rewinds stay on this line", not "only this round's work
is discarded". `guard_protected` needs no such check on *which paths* it may touch: every path it
restores or removes is an explicit pathspec filtered through the protected set, so it cannot reach
outside it.

## Cutting a release

These five drive [`examples/release.flux`](https://github.com/codewandler/flux/blob/main/examples/release.flux),
which cuts a flux release as a Flux-Lang program. They exist as separate, narrow ops rather than as
`proc.run` calls because a release flow needs exactly two programs and three writable files: fixed
argv and a fixed path set are authority you cannot mistype.

| op | arguments | description |
|---|---|---|
| `release_plan` | | The last `v*` tag, the commit subjects and diffstat since it, and the **host's** bump decision + next version |
| `release_verify_versions` | | Run `scripts/check-crate-versions.sh` (fixed argv); errors with the offending protocol-line crate named |
| `release_parse_notes` | `text` | Strictly parse the scribe's textual JSON contract into typed release-note fields; normalizes one canonical `json` fence, rejects surrounding prose and schema drift; pure, with no external authority |
| `changelog_insert` | `file, body, [section], [apply]` | Insert markdown under a changelog's `## [<section>]` heading, deterministically and idempotently. `apply` defaults to false (preview) |
| `release_cut` | `bump, [apply]` | Cut with `scripts/cut-release.sh <bump>` (fixed argv), stopping at the **local** annotated tag. `apply` defaults to false |

The division of labour is the point: **the model writes prose, the host decides the version.**
`release_plan` derives the bump from conventional-commit titles — a `!` means breaking, and breaking
means a minor bump while flux is `0.y` — so no version is ever read back out of a model reply.
crates.io is yank-only, and a wrong version cannot be withdrawn. A model may return a `bump_opinion`
and disagree in writing; the run surfaces the disagreement and cuts the host's number anyway.

`task()` returns text, even when the prompt requests JSON. `release_parse_notes` is the explicit host
boundary that accepts only the exact release-note object before the flow reads any of its fields.

`changelog_insert` addresses only `CHANGELOG.md`, `WHATS-NEW.md`, and `website/docs/whats-new.md`,
resolving its target through the canonicalizing IO boundary first — so the model's prose is an *input*
to the file, never the file's contents. `release_cut` never pushes and never publishes: the tag it
leaves is local, and the existing tag-triggered workflows promote it.

## The loop itself

flux's own agent turn loop is a Flux-Lang flow. Its hidden adaptive stages are `detect_intent`,
`explore`, `approve_batch`, `execute_batch`, and `present_results`; they are never advertised to the
model. See [The agent loop](../agent/agent-loop.md).

Whatever the op, the rule is the same: **every** call crosses authorization, approval, and
guarded IO. There is no trusted shortcut for any operation on this page.

## Related docs

- [Node reference](./node-reference.md) — the `call` node and its JSON shape.
- [Safety & approvals](../agent/safety.md) — the envelope every operation crosses.
- [Datasources](../agent/datasources.md) — the knowledge layer behind `sources`/`search`/`get`/`list`/`relation`/`batch_get`.
- [Plugin authoring](../plugins/authoring.md) — how plugins project new operations.
