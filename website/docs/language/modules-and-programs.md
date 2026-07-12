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

**Reuse across sessions — the flows home.** Beyond the module they are declared in, composite ops
saved as `.flux` files under `.flux/flows` (project) or `~/.flux/flows` (global) **auto-load as
callable ops** on every run — so a `~/.flux/flows/greet.flux` that defines `op greet(...)` is callable
by name anywhere. Agents use `flow_list` / `flow_run`; from a terminal, `flux flow list` shows the
same catalog and `flux flow run <name>` runs a saved flow directly. Supply declared parameters with
`--inputs '{"key":"value"}'` or repeatable `--arg key=value`; opt in to natural-language mapping only
when wanted with `--map-inputs "…"`.

The complete discovery, precedence, CLI-input, and `op.register` scope rules live in
[Saved flows and custom operations](../agent/saved-flows.md).

## Program declarations

Beyond flows and ops, a module may declare a whole multi-agent application: `permissions`, `agent`,
`channel`, `datasource`, `trigger`, and `journey` declarations describe the capability ceiling,
agents, channels, indexed data, and event-triggered journeys that tie them together — one typed
`.flux` file for the entire app. A journey's optional `agent` attribute makes ownership executable:
the fixed flow inherits that agent's model, persona, datasource scope, and capability narrowing.
Secrets are declared as references
(`secret "ENV_VAR"`) and resolved at load time; values never live in the file.

Top-level and agent `allow`/`deny` lists contain exact operation names. The app list is the ceiling;
an agent list may narrow but never widen it. `tools` remains the separate model-visible catalog.

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
- [Saved flows and custom operations](../agent/saved-flows.md) — project/global reuse and dynamic registration.
