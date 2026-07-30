# Flux-Lang — Compact Glossary

## Node kinds

### Primitive
| Node | What it does |
|---|---|
| `call` | Invoke a registered op with argument expressions |
| `bind` | Bind a result to a `$name` symbol |
| `memo` | Like `bind`, compute-once-per-session (pinned across turns) |
| `peek` | Read a session symbol's current value (no IO) |
| `var` | Reference a bound symbol |
| `lit` | A raw JSON literal value |
| `thing` | Reference an external object (file, ticket, person, …) |

### Pure computation (no IO, no model)
| Node | What it does |
|---|---|
| `expr` | `"price * 2"` over named vars — arithmetic, comparison, boolean, string functions |
| `jq` | `".bitcoin.usd"` JSON path extraction on a value |
| `fmt` | `"Hello {name}"` — template interpolation with `{symbol}` |
| `parse` | String → `f64` / `i64` / `bool` / `json` / `string` |
| `obj` | Record constructor `{k: expr, …}` |
| `list` | List constructor `[expr, …]` |
| `ctx` | Build a bounded, budgeted context pack from existing symbols (no IO) |
| `ctx_append` | Accrete more symbols into an existing context pack (the `+=` marker), rebinding immutably |

### Sequencing
| Node | What it does |
|---|---|
| `seq` | Run body in order, optionally bind final result |
| `pipe` | Chain calls: each step's output feeds the next step's first argument |

### Conditional
| Node | What it does |
|---|---|
| `when` | If cond is truthy, run `then`; optionally `otherwise` |
| `unless` | Run body only when cond is falsey |
| `match` | Exhaustive switch by JSON equality — `match $x` with indented `case "a"` arms + optional `default` |
| `route` | Model-routed branch — the model chooses which case runs (bounded non-determinism) |

### Guards
| Node | What it does |
|---|---|
| `assert` | Abort flow if cond is falsey (with optional message) |
| `verify` | Run a command and assert its output contains an expected substring; abort with a structured error otherwise |

### Loops
| Node | What it does |
|---|---|
| `repeat` | Counter loop — `repeat 3, until: $done` (`until` also accepted as the first body line) |
| `each` | List iteration — `each $f in $files -> $results`; the `-> $name` collects results |
| `loop` | Time-bounded polling — `loop for 30s, every: 1s, until: $done` |

### Error & recovery
| Node | What it does |
|---|---|
| `try` | Run body; on error bind it to `$err` and run handler |
| `retry` | Same body up to `max` times on failure |
| `repair` | *(planned)* On failure, model sees error + evidence and emits a corrected fragment |
| `fallback` | Ordered selector — try branches in turn; first non-empty success wins |
| `saga` | Compensating transaction — run steps; on later failure undo in reverse order |
| `scope` | RAII — acquire → use → finally (cleanup always runs) |

### Concurrency
| Node | What it does |
|---|---|
| `parallel` | Fan-out — run independent branches concurrently, bind each result to its name |
| `race` | First-wins — run branches in parallel; first success wins; `timeout_ms` required |

### Rate & cost control
| Node | What it does |
|---|---|
| `throttle` | Rate-limit — max `n` dispatches per sliding `window_ms` |
| `debounce` | Coalesce — wait `wait_ms` after last trigger before running body |
| `timeout` | Deadline — abort body if not finished in `ms` |
| `budget` | Cost cap — at most `limit` op dispatches inside body |

### Security
| Node | What it does |
|---|---|
| `cap_scope` | Capability gate — `with_tools ["read","grep"]` restricts what ops body can call |
| `confirm` | Human-in-the-loop gate — pauses for TUI/modal approval |

### Persistence & lifecycle
| Node | What it does |
|---|---|
| `once` | At-most-once — idempotency label; runs body only once per session |
| `checkpoint` | Durable resume point — top-level only; re-run skips completed prefix |
| `await` | Suspend until an external event on a named source (top-level only, v1) |
| `return` | End the flow with a value |

---

## Prelude / artifact types

| Type | What it is |
|---|---|
| `Span` | A cited region in a source `{source: ThingRef, range}` |
| `Claim` | A factual assertion with provenance `{text, span?, confidence}` |
| `Evidence` | A claim plus the spans grounding it `{claim, support: [Span]}` — the `{kind, phase, data}` bag is `flux_evidence::Observation`, a different thing |
| `Need` | A requirement `{ask, require, done_when?}` |
| `Ctx` | A budgeted context pack `{name, purpose?, members, budget?}` — selected symbols, auto-trimmed to char budget |
| `Query` | A structured search `{find, near?, type?, sources, after?, limit?}` |
| `Answer` | A successful, evidence-bearing result `{status, summary, evidence, gaps, risks}` |
| `Blocked` | A task that could not be completed `{status, summary, evidence, gaps, risks}` — same shape as `Answer`, distinct type |
| `Patch` | A proposed code change `{path, diff}` |
| `TestResult` | A test outcome `{ok, failures, summary}` |
| `Verdict` | A judgment `{choice, reasons, evidence}` |

---

## Key concepts

| Concept | Meaning |
|---|---|
| Draft AST | The untyped, possibly-unsafe AST the model emits — `thing` refs, unresolved names |
| HIR | Typed, lowered intermediate representation — all symbols resolved |
| PhysicalPlan | Optimized, flattened plan — the optimizer's output (the analyzer's output is the typed `HirFlow`) |
| Safety envelope | All effects go through: policy → approval → guarded IO → redaction |
| `$name` | Session symbol — bound by `bind`/`memo`/`each`/`parallel` |
| `{name}` | String interpolation — expanded in `Lit` strings and `fmt` by the runtime |
| FlowEffect | Semantic effect label: `pure` / `read` / `model` / `network` / `write_file` / … |
| `!model` op | *(planned)* LLM-as-a-node — model call dispatched like any other op |
| EmissionArm | JSON (default, model emits `DraftAst` schema) vs. Text (model writes `.flux` source) |

---

## Text syntax at a glance

```flux
flow at-a-glance
  $x     = read("file.txt")
  $total = $x.total
  when $total
    return {ok: true, amount: $total}
  else
    return {ok: false, reason: "empty"}
```
