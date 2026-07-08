---
title: Modules, composite ops & programs
description: Multi-flow .flux files, reusable composite op declarations, and whole multi-agent programs declared in one file.
---

# Modules, composite ops & programs

A `.flux` file can hold one flow, a reusable module, or an application declaration. This page covers
the module-level surface: multiple flows, composite `op` declarations, and the program declarations
that the app host understands.

## Multi-flow modules

A file with several flows is a **module**. Each `flow` header sits at column 0; blank lines
and comments between flows are allowed:

```flux
flow fetch-and-grep
  $hits = grep({pattern: "TODO", glob: "*.rs"})
  return $hits

flow summarize(text: String) -> String
  $summary = task({role: "summarizer", task: "Summarize:\n{text}"})
  return $summary
```

Because the `flow` header is always required, any single-flow snippet is valid in a multi-flow
file without modification.

## Composite ops

A module may declare reusable custom operations with `op`. A composite op has typed
parameters, optional metadata, and an ordinary Flux-Lang body — it is callable like any other
op from flows in the same module:

```flux
op repo-health(path: String, prior: Ctx) -> Health
  description "Check git state and summarize failures"
  risk "medium"
  idempotency "idempotent"
  effects [read, process, local_system]
  expose true

  $status = git_status()
  $tests = cargo_test({args: ["--workspace"]})
  ctx $pack
    purpose "repo-health"
    budget 8000
    include $prior, $status, $tests
  return {status: $status, tests: $tests}
```

The metadata lines, all optional:

| key | meaning |
|---|---|
| `description "…"` | what the op does — shown in catalogs |
| `risk "…"` | declared risk level |
| `idempotency "…"` | whether re-running is safe |
| `effects [...]` | declared semantic effects |
| `limits {...}` | declared operational limits |
| `expose true` | offer the op beyond this module |
| `view "…"` | display hint |

Rules that keep composite ops safe and analyzable:

- **The safety envelope still applies.** A composite op is a scoped sub-flow; its inner calls
  dispatch through authorization, approval, and guarded IO exactly like top-level calls.
  Wrapping an operation in an `op` never launders its risk.
- **`await` is rejected** inside composite ops.
- **Recursion is invalid** — direct or indirect. Composite ops are compositions, not general
  functions.

Composite ops are how a module grows a vocabulary: name a multi-step pattern once, call it
like a built-in everywhere else.

## Program declarations

Beyond flows and ops, a module may declare a whole multi-agent application: `agent`,
`channel`, `datasource`, `trigger`, and `journey` declarations describe agents, the channels
they listen on, the data they index, and the event-triggered journeys that tie them together —
one typed `.flux` file for the entire app. Secrets are declared as references
(`secret "ENV_VAR"`) and resolved at load time; values never live in the file.

Programs are run by the app host:

```bash
flux app run program.flux      # or: flux run program.flux (auto-detected)
```

The declarations, the app runtime, the event bus, and a runnable example live in
[Multi-agent programs](../agent/programs.md). The extra operations the app host registers for
journeys (`emit`, `send`, `ask`, `spawn`) are listed in [Operations](./ops.md).

## Related docs

- [Multi-agent programs](../agent/programs.md) — how program declarations run in the app host.
- [Flows & syntax](./flows-and-syntax.md) — the base syntax used inside modules.
- [Operations](./ops.md) — app-host operations such as `emit`, `send`, `ask`, and `spawn`.
