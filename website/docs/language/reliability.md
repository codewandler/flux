---
title: Reliability & guard rails
description: The Flux-Lang guard-rail nodes — assert, retry, timeout, budget, with_tools, try, confirm, verify, throttle, and debounce — with examples and exact semantics.
---

# Reliability & guard rails

Reliability constraints are first-class plan nodes, not instructions buried in a prompt. A retry
policy, deadline, dispatch cap, or approval gate is visible before execution and enforced by the
runtime during execution.

All ten guard rails have native text spellings: `assert`, `retry`, `timeout`, `budget`,
`with_tools`, `try`, `confirm`, `verify`, `throttle`, and `debounce`. Full field tables live in the
[node reference](./node-reference.md).

## `assert` — abort on a falsey condition

```flux
$hits = grep({pattern: "ERROR", glob: "*.log"})
assert $hits, "no ERROR lines found"
assert len($hits) > 0, "no ERROR lines found"
```

`assert <cond>` aborts the flow with an error when the condition is falsey; execution never continues past a failed assert. The optional message follows the first top-level comma and becomes the error detail. The condition may be a symbol, a call, a literal, or a native expression such as `$score >= 0.8`, and it uses the same truthiness rules as `when` — see the [execution model](./execution-model.md). Use it to fail fast instead of writing a `when` around a manual error return.

## `retry` — retry transient failures

```flux
retry 3 backoff exponential delay 500 -> $health
  web.fetch("https://api.example.com/health")
```

The header uses space-keyword tokens in a fixed order:

| token | required | meaning |
|---|---|---|
| `<max>` | yes | maximum attempts, including the first |
| `backoff none / linear / exponential` | no | inter-attempt delay strategy (default `none`) |
| `delay <ms>` | no | base delay in milliseconds |
| `-> $bind` | no | binds the body's last expression on success |

The backoff schedule, where `k` counts retries (`k = 1` is the wait before the second attempt):

| strategy | wait before retry `k` |
|---|---|
| `none` | `delay` |
| `linear` | `delay × k` |
| `exponential` | `delay × 2^(k−1)`, with the multiplier capped at `2^10` |

Semantics to keep in mind:

- **Fatal errors are never retried.** A policy denial, an unknown op, or a type error propagates immediately — retrying cannot fix them.
- **A denied `confirm` is not retried.** A human "no" inside the body is an answer, not a transient failure.
- **Bind through the header, not inside the body.** The `-> $bind` captures the body's last expression on success; do not also bind the same result inside the body.
- After `max` failed attempts, the node errors with the last attempt's error message.

```flux
# correct: the header captures the result
retry 3 -> $out
  bash("flaky.sh")

# also correct: side effects only, nothing bound
retry 3
  bash("flaky.sh")
```

## `timeout` — bound wall-clock time

```flux
timeout 5000 -> $page
  $page = web.fetch("https://example.com")
```

`timeout <ms>` runs its body under a wall-clock deadline. If the body finishes in time, `-> $bind` names its result. If the deadline expires, the node errors — and that error is catchable by an enclosing `try` or `retry`, so a slow path can degrade instead of killing the flow.

Dispatches that completed before the deadline stay counted and traced; a timeout does not erase the work that already happened, it only stops what comes after.

## `budget` — cap op dispatches

```flux
budget 10 -> $notes
  $hits  = grep({pattern: "TODO", glob: "*.rs"})
  $notes = ai.reason({ask: "Cluster these TODOs: {hits}"})
```

`budget <n>` caps the number of **op dispatches** inside its body — calls that go through the runtime's dispatch gate. Pure nodes (`fmt`, `jq`, `expr`, value templates) dispatch nothing and are free.

The cap is checked at statement boundaries. A single nested statement — an `each` over a long list, say — can consume several dispatches before the next check, so a scope can overshoot its limit by the width of one statement. Treat the budget as a firm brake, not an exact meter.

v1 counts dispatches, not tokens or money. `-> $bind` names the body's result.

## `with_tools` — capability scope

```flux
with_tools ["read", "grep"] -> $hits
  $src  = read("src/lib.rs")
  $hits = grep({pattern: "unwrap", glob: "*.rs"})
```

`with_tools [...]` restricts op dispatch inside its body to the named tools. A call to anything outside the allowlist **fails closed** at the runtime's dispatch gate — even when the surrounding session policy would have allowed it. This is a runtime-enforced capability boundary, not an advisory hint.

- **Capabilities only narrow on descent.** Nested `with_tools` scopes are intersected: an inner block can never re-grant a tool an outer block removed.
- **The analyzer echoes the rule statically.** A literal call to a tool that is provably absent from the list is flagged before the flow runs; dynamic dispatch is still caught at runtime.
- `-> $bind` names the body's result.

Use it to hand a sub-plan read-only capabilities, or to guarantee a model-influenced section cannot reach `bash` or `write` no matter what it emits. See [Safety & approvals](../agent/safety.md) for the session-level policy this composes with.

## `try` — catch and handle errors

```flux
try
  bash("might-fail.sh")
catch $err
  bash("echo fallback: {err}")
```

- The body runs first. If it succeeds, the handler never runs.
- On failure, the error **string** is bound to the `catch` symbol (here `$err`) and the handler runs — the handler can interpolate `{err}` or branch on it.
- If the handler itself errors, that error propagates.
- The `catch $err` arm (and its handler block) is optional. A `try` with no handler suppresses errors **silently** — use that deliberately, or not at all.

## `confirm` — human approval gate

```flux
confirm "Delete all temporary files?" risk high
  bash("rm -rf tmp/")
```

- `message` is required. `risk` is one of `low` / `medium` (default) / `high` / `critical`.
- The gate calls the session approver: the TUI shows a modal, `--yes` auto-approves, and the plain CLI prompts interactively. See [Safety & approvals](../agent/safety.md).
- The body runs **only on approval**; a denial makes the node error immediately. A denied `confirm` inside a `retry` is not retried.
- A `confirm` with no body is valid — a pure gate that pauses the flow for a decision without a conditional action (`confirm "Proceed?"` on its own line).
- The approver sees the risk prepended to the message, as `[high] Delete all temporary files?`, so the severity is visible where the decision is made.

## `verify` — assert on command output

```flux
verify bash("cargo test --workspace 2>&1") contains "test result: ok": "workspace tests failed"
```

- Runs the command (any expression producing a string — typically a `bash` call), then checks that the output **contains** the expected substring.
- If the substring is missing, the flow aborts with a structured error; the optional `: "message"` suffix overrides the default error text.
- Use it after an edit or build to guard against silent failure. Wrap it in a `try` if you want to handle a failed check gracefully rather than aborting.

## `throttle` — rate-limit dispatches

```flux
throttle "fetches" 5 per 60000
  web.fetch($url)
```

- The header reads `throttle "<name>" <max> per <window_ms>`: at most `max` op dispatches inside the body per sliding `window_ms` window.
- The token bucket is keyed by `(session, name)` and updated **atomically**, and it survives across turns. Two `throttle` nodes with distinct names never share a bucket; reusing a `name` deliberately shares one.
- When the limit is exceeded the node **errors instead of blocking**, so the plan stays responsive. Wrap it in `try` or `retry` (with a delay) if waiting is the right response.

## `debounce` — coalesce bursts across turns

```flux
debounce "rebuild" 300
  bash("rebuild.sh")
```

- The header reads `debounce "<name>" <wait_ms>`. Each time the node is reached, a last-trigger timestamp for its `name` is recorded in the session store. The body runs only once `wait_ms` has elapsed since that key's last trigger.
- Re-arrivals inside the window re-arm the timer, so a burst of triggers coalesces into a single body run after things settle.
- Because the timestamp lives in the session store keyed by `(session, name)`, the settling window spans turns — not just one plan execution.

## Related docs

- Ordered graceful degradation with `fallback` — [Control flow](./control-flow.md)
- First-success `race` and fan-out `parallel` — [Concurrency](./concurrency.md)
- Guaranteed cleanup (`scope`), rollback (`saga`), and at-most-once effects (`once`) — [Durability & cross-turn state](./durability.md)
- The session policy and approval chain every dispatch passes through — [Safety & approvals](../agent/safety.md)
