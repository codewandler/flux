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

Arguments below are named; pass them as a single object (`read({path: "…", limit: 100})`), or
bare for a sole required parameter (`read("README.md")`). Optional arguments are in
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
| `search` | `query[, source, entity, limit]` | low | Keyword search over the indexed [datasource](../agent/datasources.md) |
| `get` | `source, entity, id` | low | Fetch one datasource record in full by its address |
| `list` | `source[, entity, offset, limit]` | low | Enumerate a datasource source's records, paged |
| `relation` | `source, entity, id[, rel]` | low | Follow a datasource record's typed links |
| `batch_get` | `source, entity, ids` | low | Fetch several datasource records in one call |
| `web.fetch` | `url[, raw]` | low | Read a URL as a document: HTML becomes condensed Markdown, PDFs become extracted text; `raw` preserves the body |
| `web.crawl` | `url[, max_pages, max_depth, max_total_bytes]` | low | Crawl a small site or section: from a seed, follow same-host links breadth-first (bounded by `max_pages`/`max_depth`, and optionally a total-content `max_total_bytes` budget that stops the crawl early), returning each page as condensed Markdown |
| `html_to_markdown` | `html` | low | Pure conversion of an HTML string to condensed Markdown; no network access |
| `http.request` | `url[, method, headers, body, timeout]` | medium, approval | Arbitrary HTTP request with capped response body; non-2xx remains a result |
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
| `git_stage` / `git_unstage` | `paths` | medium / low | Stage or unstage files |
| `git_commit` | `message[, body]` | medium | Create a commit |
| `git_push` | `[branch, remote]` | medium | Push to a remote |
| `git_checkout` | `branch[, create]` | medium | Switch or create a branch |

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

**Examples:**

```flux
// Deterministic list transforms
$authors = map({items: $issues, path: "author.username"})
$open = filter({items: $issues, where: "it.state == 'opened' && it.upvotes > min", vars: {min: 2}})
$all_pages = flatten($pages)
$rest = skip({items: $candidates, n: 1})
$report = join({items: $lines, sep: "\n"})
$hosts = split({s: $raw_hosts, sep: ",", trim: true})
$total = sum({items: $invoices, path: "amount"})
$by_status = count_by({items: $issues, path: "state"})
$grouped = group_by({items: $issues, path: "author.username"})
$has_bug = has({items: $labels, value: "bug"})
$all_green = all({items: $checks, where: "it.status == 'ok'"})
$slim_issues = pick({items: $issues, keys: ["iid", "title", "state", "web_url"]})
$public_issue = omit({items: $issue, keys: ["author_email", "raw_payload"]})
$merged = merge_obj([ $defaults, $overrides ])
$assignee = coalesce({values: [$issue.assignee.username?, $issue.author.username?], default: "unassigned"})
$field_names = keys($issue)
$field_values = values($issue)

// Check if a log line contains ERROR
$has_error = regex_match({s: $log_line, pattern: "ERROR"})
when $has_error
  alert({msg: "Error detected"})

// Extract SemVer from a version string
$version = regex_extract({
  s: "flux-cli v1.2.3", pattern: r"v(\d+\.\d+\.\d+)", group: 1
})  // returns "1.2.3"

// Extract all email addresses from text
$emails = regex_extract({
  s: $body_text,
  pattern: r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b",
  all: true
})  // returns an array of strings
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

## App orchestration ops

Registered **only** by the `flux app run` host for [multi-agent programs](../agent/programs.md)
— journeys use them to drive the event bus and channels:

| op | arguments | description |
|---|---|---|
| `emit` | `event[, payload]` | Publish an event to the bus (fires matching triggers) |
| `send` | `channel, message` | Send a message to a named channel |
| `ask` | `channel, message` | Send and return a correlation id |
| `spawn` | `run[, input]` | Run a named journey to completion and return its result |

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

## Flows

Discover and run reusable flows and composite ops stored under `.flux/flows` (project) and
`~/.flux/flows` (global) — see [Where flows live](./tooling.md#where-flows-live):

| op | arguments | description |
|---|---|---|
| `flow_list` | | List the flows and composite ops in the flows home, each with its description and params |
| `flow_run` | `name[, inputs]` | Run a stored flow by name; an `inputs` object is seeded as `$key` binds, then it runs in the current session |
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

## The loop itself

flux's own agent turn loop is a Flux-Lang flow, driven by reflexive planning and evidence ops
that are never advertised to the model — see [The agent loop](../agent/agent-loop.md).

Whatever the op, the rule is the same: **every** call crosses authorization, approval, and
guarded IO. There is no trusted shortcut for any operation on this page.

## Related docs

- [Node reference](./node-reference.md) — the `call` node and its JSON shape.
- [Safety & approvals](../agent/safety.md) — the envelope every operation crosses.
- [Datasources](../agent/datasources.md) — the knowledge layer behind `sources`/`search`/`get`/`list`/`relation`/`batch_get`.
- [Plugin authoring](../plugins/authoring.md) — how plugins project new operations.
