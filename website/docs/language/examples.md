---
title: Examples
description: A cookbook of complete Flux-Lang flows plus clearly labeled illustrative case studies.
---

# Examples

The first examples are complete, parser- and formatter-checked flows. You can paste one into a
`.flux` file and run it once the named operations and any required provider or credentials are
configured. Each stays small enough to highlight one pattern. The final two sections are explicitly
illustrative case studies; their current blockers are stated where they appear.

## Read and summarize

One read, one budgeted context pack, one model call:

```flux
flow summarize-readme
  src = read("README.md")
  ctx brief
    purpose "summarize the project README"
    budget 6000
    include src
  summary = ai.reason(ask: "Summarize the project in five bullets.", ctx: brief)
  return summary
```

## Fetch, extract, format

Pure field access and formatting — no shell, no approval pauses:

```flux
flow latest-release
  raw = web.fetch("https://api.github.com/repos/codewandler/flux/releases/latest")
  tag = raw.tag_name
  msg = fmt("latest flux release: {tag}")
  return { message: msg, tag }
```

## Bounded routing

A selector picks among declared branches; the case set is fixed before anything runs:

```flux
flow route-ticket(ticket: String)
  route classify(ticket)
    case "bug"
      queue = "engineering"
    case "billing"
      queue = "finance"
    default
      queue = "support"
  msg = fmt("routed to {queue}")
  return msg
```

## Resilient fetch

Cache first, then the network with backoff — the first branch that succeeds with a non-empty
result wins:

```flux
flow cached-page(url: String)
  fallback -> page
    branch
      page = read("cache/page.html")
    branch
      retry 3, backoff: exponential, delay: 500ms -> page
        web.fetch(url)
  assert page, "no cached copy and the fetch failed"
  return page
```

## Fan out, then reason once

Independent reads run concurrently; one model call sees a budgeted pack of all three results:

```flux
flow repo-survey
  parallel
    branch readme
      readme = read("README.md")
    branch todos
      todos = grep(glob: "*.rs", max_results: 100, pattern: "TODO")
    branch status
      status = git_status()
  ctx pack
    purpose "assess repository state"
    budget 8000
    include readme, todos, status
  assessment = ai.reason(ask: "What needs attention first?", ctx: pack)
  return { assessment, todos }
```

## Poll until done

A time-bounded loop with an early-exit guard — `path_exists` returns `"true"`/`"false"`, which
plugs straight into truthiness:

```flux
flow wait-for-artifact
  loop for 1m, every: 2s, until: found -> found
    found = path_exists("target/release/flux")
  assert found, "artifact did not appear within 60s"
  return "artifact ready"
```

## Walk directories

`-> flat` concatenates per-iteration lists into one:

```flux
flow rust-files(dirs: List<String>)
  each dir in dirs -> flat files
    glob(path: dir, pattern: "*.rs")
  each f in files -> stats
    file_stat(f)
  return { files, stats }
```

## Illustrative case study: the improvement loop

This abridged flow shows the shape of flux's improvement loop — evaluate, mine pain points in
parallel, implement candidates, keep what measures better, and revert what does not. It is not
runnable verbatim: `adapter: "local"` is a narrative placeholder rather than a shipped eval adapter,
and the production flows include additional protected-path and audit steps omitted here. Use the
checked-in flows linked below when running an improvement round.

```flux
flow improve -> EvalReport
  baseline = eval_run(adapter: "local", dir: "suites", trials: 3)
  sessions = eval_sessions(baseline)
  digest = sessions_digest(sessions)
  parallel
    branch mined
      mined = painpoints_collect(sessions)
    branch reviewed
      reviewed = task(role: "reviewer", task: """Review these eval sessions for failure modes.
Sessions:
{digest}

Return ONLY a JSON array of findings.""")
  candidates = improvements_aggregate(mined, reviewed)
  repeat 3, until: done
    tasks = task(role: "planner", task: """Turn these candidates into AT MOST 2 tasks:
{candidates}""")
    snapshot = git_snapshot()
    change_implement(limit: 2, tasks)
    gate = gate_check()
    when gate
      candidate = eval_run(adapter: "local", dir: "suites", trials: 3)
      when score_compare(baseline, candidate)
        git_stage(["."])
        git_commit("improve: adopt candidate")
        baseline = eval_adopt(candidate)
      else
        git_reset(snapshot)
    else
      git_reset(snapshot)
    done = candidates_empty(candidates)
    candidates = candidates_advance(candidates)
  return baseline
```

Everything here is ordinary language surface: `parallel` fan-out, a bounded `repeat` with an
`until` guard, nested `when`/`else`, and every op — including the sub-agent `task` calls —
crossing the safety envelope.

## Blocked case study: Zendesk triage

[`examples/zendesk.triage.flux`](https://github.com/codewandler/flux/blob/main/examples/zendesk.triage.flux)
is a multi-flow module with four one-shot entrypoints. Authored control flow owns retry, concurrency,
timeouts, context budgets, and fallback; the model only analyzes bounded ticket evidence.

:::caution Not runnable against a live Zendesk account yet
The `zendesk` plugin these flows called was removed before its first release. The operations are now
served by flux-connectors' connector pack, which a host registers when it builds its client — and the
operation names did not have to change, because the pack was authored to this flow's shape. The
commands below are future invocation examples and currently cannot reach a live account: the Zendesk connector declares no credential
address, and its `https://{subdomain}.zendesk.com` base URL is not yet resolved from config. Both
refuse rather than sending a broken request. The module remains a worked example of the
authored-control-flow shape, which is what this page is illustrating.
:::

```bash
flux run examples/zendesk.triage.flux --entry setup --yes
flux run examples/zendesk.triage.flux --entry triage \
  --arg 'query=type:ticket status:new' --yes
flux run examples/zendesk.triage.flux --entry brief --arg ticket_id=12345 --yes
flux run examples/zendesk.triage.flux --entry eod \
  --arg 'query=type:ticket updated>24hours' --yes
```

The module is read-only in a precise sense: registering the pack brings the connector's three write
operations into the host's registry too, and what is guaranteed — and asserted against the module's own
call graph — is that no entrypoint here reaches one. Keeping them unreachable altogether is the host's
approval decision. Provider failure falls back to the gathered response instead of losing the
deterministic work.

## Going further

- The repository ships runnable examples in
  [`examples/`](https://github.com/codewandler/flux/tree/main/examples), including the real
  improvement loops.
- A single `.flux` file can also declare agents, channels, and journeys — a whole application.
  See [Multi-agent programs](../agent/programs.md) and
  [Modules, composite ops & programs](./modules-and-programs.md).

## Related docs

- [A ten-minute tour](./tour.md) — learn the examples one construct at a time.
- [Tooling](./tooling.md) — run, preview, format, and compile flows.
- [Modules, composite ops & programs](./modules-and-programs.md) — scale a flow into an app.
