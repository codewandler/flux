# Design: named-argument calls (deprecate positional binding)

Companion to story `L-09-named-argument-calls.md`. The "why" and the precise semantics live here;
the story carries the acceptance + progress log.

## Problem

Today a Flux-Lang `call` binds its `args: [Node]` **positionally** to the op's parameters in
`required ++ optional` order (runtime `map_args_to_input`, analyzer `check_call_types`). Parameter
*order* is therefore load-bearing:

- `schema_params` (opspec.rs) recovers `(required, optional)` from the JSON Schema: required order
  from the `required` array, optional order from a custom `x-param-order` key (falling back to
  `properties` key order).
- `OpSpec::input_schema` emits `required` in declaration order so the round-trip is stable.
- The planner prompt tells the model "a call's `args` are positional in that parameter order."

This is the one thing blocking the schemars migration: `schemars::schema_for!(T)` derives a schema
from a Rust struct, and it **does not emit `x-param-order`**. Optional-param ordering is lost, so
ops with only optional params (the cargo/toolchain/cognition families) cannot be called
positionally once their schema is derived. Required-only ops survive (schemars emits `required` in
field order), but relying on field order for *binding* is exactly the kind of implicit convention
the codebase avoids.

## Goal

Make **parameter order non-load-bearing**. A call names its arguments; no positional binding path
needs a stable param order. This deletes `x-param-order`, lets `schema_params` treat `required` as
a *set*, and lets the schemars migration land without any ordering accommodation.

## Semantics

A `call`'s `args` is one of:

1. **Empty** `[]` — a no-param op. Unchanged.
2. **One object** `[Obj{…} | Lit{object}]` — the named input map, passed straight through. **This is
   the canonical multi-param form.** Already supported today (the lone-object passthrough in
   `map_args_to_input`); we promote it from a convenience to *the* form.
3. **One bare value** `[Lit | Var | Call | …]` — **single-arg sugar**: binds to the op's *sole*
   parameter when the op declares exactly one (required or optional). Preserves the common
   ergonomic calls: `read("README.md")`, `grep("TODO")`, `plan($feedback)`, `run_plan($plan)`,
   `fmt("…")`, `append("log.txt", …)` stays object form because `append` has 2 required params.

   When an op has **more than one** parameter, a single bare value is a *compile error*
   (ambiguous), routed through the analyzer repair loop.

**Removed:** positional binding of two-or-more bare args (`write("p", "c")`). The analyzer
emits a diagnostic telling the model to use an object. The runtime keeps a **deprecated
fallback** (bind required-then-optional by catalog order) so a stray legacy plan still runs rather
than failing mid-execution — but the planner never emits it and the analyzer flags it, so it is a
safety net, not a supported form. (This avoids a hard break of any in-flight stored plan.)

`required` is now a **set** (membership only — "these params must be present in the object").
`optional` is the complement. Neither carries binding order. `schema_params` returns
`(required: Vec<String>, optional: Vec<String>)` whose order is **display-only** (sorted, stable)
and used solely by `param_signature()` for the catalog text — never for binding.

### `pipe`

`pipe` splices the previous step's output as the first argument of the next step's `call`. Under
named-args that has no meaning. **`pipe` steps must now carry their own object arg naming the
previous output explicitly** — e.g. the runtime binds the previous output to a well-known field.
Concretely: the previous step's result is spliced into the step's object arg under the field
`$prev` is **not** added (no magic field). Instead, `pipe` is re-specified so each step is a
`call` whose object arg may reference a synthetic `__pipe` symbol — but that reintroduces a
convention.

**Decision:** keep `pipe` semantics but redefine the splice as "the previous output becomes the
**sole-arg sugar** target when the step's `call` has a single bare arg omitted." Simpler: **`pipe`
step calls omit their first (sole-required) argument and the runtime supplies the previous
output.** So `pipe { read; grep("TODO") }` runs `read(prev)` then `grep("TODO", …)` — wait, `grep`
takes a pattern (sole required) so the previous output (the haystack) can't be the pattern.

`pipe` is genuinely positional and used little. **Scope decision: `pipe` keeps its current
positional splice for the first argument of each step** (the one place order remains), documented
as an exception. This is a single, contained, well-understood convention (pipeline threading) and
does not block the schemars migration (`pipe` doesn't read `x-param-order`; it uses the `required`
order of a *single* param). Everything *else* goes named.

## What changes

| Site | Change |
|---|---|
| `opspec.rs` `schema_params` | Drop the `x-param-order` branch. `required` order = schema `required` array order (display only); `optional` = `properties` keys not in `required`, sorted. Order is non-binding. |
| `opspec.rs` `OpSpec::input_schema` | Stop relying on `required` order for binding (already just emits it; doc that order is display-only). |
| `opspec.rs` `OpSignature` | `required_params`/`optional_params` stay `Vec` for display (`param_signature`); document as sets. |
| `runtime.rs` `map_args_to_input` | Lone-object passthrough stays. Single bare value binds to the **sole** param (error via runtime if >1 param — but analyzer catches first). Multi-bare-arg positional binding becomes the deprecated fallback (catalog order), with a one-line deprecation note. |
| `runtime.rs` `literal_input` (intent preview) | Same: object passthrough; single bare → sole param; multi bare → catalog-order fallback. |
| `analyze.rs` `check_call_types` | Type-check the lone-object form by field name (already skipped today — now actively checked against `param_types`). Single bare → sole param's type. Multi bare → diagnostic "use an object arg naming the parameters". |
| `analyze.rs` arity check | `args.len() > 1 && !lone_object` → diagnostic (the positional multi-arg form is rejected). `args.is_empty() && required non-empty` stays (still a real missing-args error). |
| `flux-flow/runtime.rs` `literal_input` | (same as lang runtime) |
| Planner prompt (`compile.rs`) | Replace "args are positional in parameter order" with "pass multi-param ops a single object arg naming each parameter; single-param ops accept a bare value." Update the catalog examples. |
| `param_signature` | Render `name{a, b}` (object-style) instead of `name(a[, b])`; or keep `name(a, b)` as display-only — decision: keep parens, it's documentation. |
| Docs/skills (`reference.md`, `SKILL.md`) | Rewrite the `call` section; update examples to object form for multi-param ops. |
| Hand-authored `.flux` + examples | Migrate multi-arg positional calls to object form (`send("cli", $m)` → `send({channel:"cli", message:$m})`). Single-arg calls unchanged. |

## What does NOT change

- The `Call` AST node (`op` + `args: Vec<Node>`) — the JSON shape is identical; only the *contents*
  of `args` for multi-param calls changes (one object instead of N bare values).
- The lone-object passthrough (already the right behavior).
- Single-arg sugar (already common).
- `pipe`'s first-arg splice (contained exception).
- Provider rendering of `ToolSpec.input_schema` (schemars output is a valid object schema regardless).

## Risk

- **Model re-learning.** The planner prompt changes; models must emit object args for multi-param
  ops. Mitigation: the analyzer rejects the old form with a repair diagnostic, so a model that
  emits `write("p","c")` is told to use an object and re-emits — same loop that already repairs
  malformed plans.
- **Legacy stored plans.** A plan persisted before the change may use multi-bare-arg positional
  calls. Mitigation: the runtime deprecated fallback runs them; the analyzer flags them only at
  *compile* (new plans), not at *execute* (stored plans). No silent breakage.
- **Hand-authored flows.** Must be migrated in the same commit (small set: channels-app, hello,
  call-routing, agent-loop).
