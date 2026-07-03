---
title: Examples
---

# Flux-Lang examples

## Read and summarize

```flux
flow summarize-readme
  $src = read("README.md")
  ctx $brief
    purpose "summarize README"
    budget 6000
    include $src
  $summary = ai.reason("Summarize the project", ctx: $brief)
  return $summary
```

## Bounded routing

```flux
flow handle-ticket(ticket: String)
  $label = classify($ticket)
  route $label
    case "bug"
      return "send to engineering"
    case "billing"
      return "send to billing"
    default
      return "send to support"
```

## Resilient fallback

```flux
flow answer(query: String)
  fallback -> $answer
    branch
      $answer = cache_get($query)
    branch
      $docs = search($query)
      $answer = ai.reason("Answer from docs", ctx: $docs)
  return $answer
```

## A whole app in one file

A `.flux` file can declare more than a single flow — `agent`, `channel`, `datasource`, `trigger`, and
`journey` modules describe a complete multi-agent program. See
[Multi-agent programs](../agent/programs.md) for a runnable example and the `flux app run` runtime.
