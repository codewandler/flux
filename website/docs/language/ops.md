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
| `search` | `query[, limit]` | low | Search the indexed datasource |
| `web_fetch` | `url` | low | Fetch an HTTP(S) URL |
| `web_search` | `query[, max_results]` | low | Web search (requires a search API key) |
| `sqlite_query` | `db, sql[, params]` | low | Read-only SQLite query |
| `now` / `cwd` / `sys_info` | | low | Clock, workspace root, host metadata — no shell needed |

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
let authors = map { items: issues, path: "author.username" };
let open = filter { items: issues, where: "it.state == 'opened' && it.upvotes > min", vars: { min: 2 } };
let all_pages = flatten { items: pages };
let rest = skip { items: candidates, n: 1 };
let report = join { items: lines, sep: "\n" };
let hosts = split { s: raw_hosts, sep: ",", trim: true };
let total = sum { items: invoices, path: "amount" };
let by_status = count_by { items: issues, path: "state" };
let grouped = group_by { items: issues, path: "author.username" };
let has_bug = has { items: labels, value: "bug" };
let all_green = all { items: checks, where: "it.status == 'ok'" };
let slim_issues = pick { items: issues, keys: ["iid", "title", "state", "web_url"] };
let public_issue = omit { items: issue, keys: ["author_email", "raw_payload"] };
let merged = merge_obj { objects: [defaults, overrides] };
let assignee = coalesce { values: [issue.assignee.username?, issue.author.username?], default: "unassigned" };
let field_names = keys { item: issue };
let field_values = values { item: issue };

// Check if a log line contains ERROR
let has_error = regex_match { s: log_line, pattern: "ERROR" };
when { cond: has_error } call { op: "alert", msg: "Error detected" };

// Extract SemVer from a version string
let version = regex_extract {
  s: "flux-cli v1.2.3",
  pattern: r"v(\d+\.\d+\.\d+)",
  group: 1
};  // returns "1.2.3"

// Extract all email addresses from text
let emails = regex_extract {
  s: body_text,
  pattern: r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Z|a-z]{2,}\b",
  all: true
};  // returns array of email strings
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

## The loop itself

flux's own agent turn loop is a Flux-Lang flow, driven by reflexive planning and evidence ops
that are never advertised to the model — see [The agent loop](../agent/agent-loop.md).

Whatever the op, the rule is the same: **every** call crosses authorization, approval, and
guarded IO. There is no trusted shortcut for any operation on this page.

## Related docs

- [Node reference](./node-reference.md) — the `call` node and its JSON shape.
- [Safety & approvals](../agent/safety.md) — the envelope every operation crosses.
- [Plugin authoring](../plugins/authoring.md) — how plugins project new operations.
