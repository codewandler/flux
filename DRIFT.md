# D-31 drift report — schema↔handler mismatches found & fixed

The schemars migration makes each op's `input_schema` and its runtime parsing derive from
one typed Rust struct, so they can no longer drift. This report records the drifts the
migration surfaced — cases where the *old* hand-written JSON Schema already disagreed with
what the handler actually parses (extra/missing fields, required-vs-optional mismatches,
type mismatches). Each is now fixed by construction; the notes record what was wrong.

## Summary

- **In-process `ToolSpec` ops: ~36 sites across `flux-tools`, `flux-eval`, `flux-orchestrate`.**
  All now derive their schema from a `#[derive(Deserialize, JsonSchema)]` struct via
  `flux_spec::tool_input_schema::<T>()`. No `input_schema: json!({...})` remains (enforced by
  `crates/flux-tools/tests/no_manual_schema.rs`).
- **Handler single-source-of-truth status:** most handlers keep ad-hoc `&Value` parsing (the
  schema struct is `#[allow(dead_code)]`, schema-only). Full SSoT (handler parses the struct via
  `parse_params`) is wired for `write` (flux-tools) and `task` (flux-orchestrate). The rest are
  schema-only by design — the story's hard requirement is "no hand-written schemas"; full SSoT is
  a follow-up where the handler is a simple 1:1 field extraction.

## Drifts found

### flux-tools

- **`cargo_test` / `cargo_check` / `cargo_build` / `cargo_clippy` / `cargo_fmt`** — the old
  hand-written schemas carried a custom `"x-param-order"` array (positional binding order).
  `schemars` does not emit `x-param-order`, and L-09 (named-argument calls) made parameter order
  non-load-bearing, so the ordering extension is dropped entirely. The old
  `cargo_test_schema_declares_positional_order` test is replaced by a guard asserting the derived
  schema carries **no** `x-param-order`. **No semantic drift** — keys, optionality, and types
  match what `execute()` parses.
- **`edit`** — the old schema marked `replace_all` optional (no `"required"` entry); the
  handler reads it with `unwrap_or(false)`. The derived struct models it as `Option<bool>` with
  `#[serde(default)]` → `unwrap_or` semantics preserved. Aligned.
- **`read`** — the old schema used `oneOf` for `path` (string | array of strings); the derived
  struct models it as an `#[serde(untagged)] StringOrVec` enum, which schemars renders
  equivalently. `offset`/`limit` optional `u64` match the handler's `u64_arg(...).unwrap_or(...)`.
- **`git_*`, `patch`, `glob`, `grep`, `append`, `read_many`, `proc_run`, `bash`** — no drift:
  field names, required/optional, and types match the handler's `str_param`/`u64_arg`/array
  extraction exactly. `git_status` / `flux_reload` are no-arg ops (`struct FooInput {}`).

### flux-tools/reflect.rs

- **`op.register` — schema↔handler drift (documented, not yet unified).** The schema is derived
  from `RegisterCompositeInput` (`scope: RegisterScope` enum → lowercase `turn|session|project|
  global` in the schema), but the runtime parses a separate `CompositeRegisterRequest`
  (`flux-runtime`, `scope: String`). The schema is **richer** (enumerates allowed scope values;
  the runtime accepts any string and validates later). This is a benign drift — the schema is
  strictly more informative than the runtime type — and is recorded here rather than unified,
  because unifying would require deriving `JsonSchema` on `CompositeRegisterRequest` (in
  `flux-runtime`, an L2 crate) and losing the enum documentation. Tracked as a follow-up.
- **`plan` / `run_plan`** — validate-only structs (`PlanInput`, `RunPlanInput`): the schema is
  derived from them but `execute()` forwards the raw object to the host (the host seeds the
  planner from the object directly). No drift; the structs exist to give the model a typed schema.

### flux-eval

- **ops/git/gate/aggregate** — schemas migrated to typed structs; handlers keep the
  `coerce_json`/`arg` convention (JSON-string coercion: a `$var` arrives as a JSON-encoded
  string and is parsed on use). **Single-source-of-truth is deliberately deferred** here: a
  blanket `from_value` deserialize would break the coercion convention (a JSON-string arg would
  fail to deserialize as the struct). The structs are `#[allow(dead_code)]` schema-only. No
  schema↔handler drift was found — field names/optionality/types align with `arg`/`str_field`
  extraction.
- **`improvements_aggregate` / `change_implement` / `score_compare` / `score_compare_multi`** —
  the old schemas used dangling `$ref`s to `#/$defs/...` definitions that were **not present** in
  the input schema (broken refs the model would have seen as unresolved). The derived schemas use
  concrete `String` / `Vec<Value>` types. This is a **real fix**: the model now sees a valid,
  resolvable schema instead of a dangling reference.

### flux-orchestrate

- **`task`** — full single-source-of-truth (`TaskInput { role, task }` + `parse_params`). No
  drift; the old schema matched the handler.

## Out of scope (deferred / correctly non-manual)

- **Plugin `OperationSpec` ops (~275 across 18 plugins)** — built via host-kit's
  `so(json!({...}), json!([...]))` helper. Same migration pattern, but on the plugin side: add
  `read_op_typed`/`write_op_typed` to host-kit, `schemars` to each plugin crate, replace every
  `so(...)` + handler `Value` parsing with a typed struct. Tracked as a separate story.
- **`flux-lang/src/opspec.rs`** — the composite-op `OpSpec → JSON Schema` *generator* (it
  *produces* schemas programmatically; that's its job, not a hand-written op schema).
- **Provider `json!({"type":"object"})` sites** (`flux-providers` tests / MCP passthrough
  `ToolDef`s) — not real `ToolSpec` op declarations.
- **`flux-cli/plugin_skill.rs`** + **`flux-plugin/bin/*`** — test/example code, not registered ops.

---

# D-36 drift report — plugin OperationSpec schema↔handler mismatches

The plugin-side continuation of D-34: each migrated plugin's `OperationSpec.input_schema` is
derived from a typed `#[derive(Deserialize, schemars::JsonSchema)]` struct via
`host_kit::read_op_typed::<T>` / `write_op_typed::<T>`, instead of a hand-written
`so(json!({...}), json![...]))` literal. The structs are schema-only (handlers keep their
existing extractors — the D-34 schema-only precedent); schemars' `Option<T>` → `["T","null"]`
representation is the repo-wide convention (D-34 already adopted it on the crate side), so the
derived JSON is **demonstrably equivalent**, not byte-identical, to the legacy literal. The
contract (fields, required set, base types, enum value sets) is asserted per migrated plugin by
an inline `schema_contract` test, and a workspace guard
(`plugins/host-kit/tests/no_manual_plugin_schema.rs`, scoped to `MIGRATED_PLUGINS`) fails on a
reintroduced `so(json!{...})`.

This section records the drifts the migration surfaced in each plugin — cases where the *old*
hand-written schema already disagreed with what the handler actually parses. Each is preserved
as-is (the struct encodes the legacy schema verbatim) so the migration is a pure schema-source
change, **not** a contract change; fixing the drift is a separate story.

## Migrated so far

- **`homer`** (8 ops). Guard-scoped. Contract test: `homer` `schema_contract::*`.

### homer

- **`homer.call.list`** — handler drift (handler wider than schema). `op_call_list` shares
  `build_search_filters` with `homer.search`, so it also reads `ua`, `method`, and `call_id`
  from the input — but the legacy `homer.call.list` schema never advertised those fields. The
  model therefore cannot filter `call.list` by `ua`/`method`/`call_id`; those reads are silent
  no-ops for this op. The `CallListInput` struct omits them to preserve the contract.
- **`homer.call.show`** — handler drift (schema wider than handler). The legacy schema
  advertises a `render` field (`enum: ["svg"]`) but `op_call_show` never reads it — the op
  always renders the SVG ladder unconditionally. `CallShowInput` keeps `render: Option<Render>`
  so the derived schema still advertises it (dead param), preserving the contract.
- All other `homer` ops (`test`, `search`, `call.qos`, `call.analyze`, `pcap.export`,
  `alias.list`): no schema↔handler drift — field sets and `required` match the handler reads.

## Representation notes (not drift — expected schemars behaviour)

- Optional fields serialize as `{"type": ["<T>", "null"]}` and are omitted from `required`
  (schemars 0.8 default). The legacy `so(...)` form wrote `{"type": "<T>"}` and `"required": []`
  explicitly. These are semantically equivalent for flux's runtime, which does not validate
  input against the schema (handlers parse leniently and ignore unknown keys).
- Enums serialize as a top-level `definitions` entry referenced by `$ref` (and wrapped in
  `anyOf` with `null` when the field is `Option<Enum>`), instead of an inline
  `{"type":"string","enum":[...]}`. The enum value set is unchanged.
- Empty-input structs serialize as `{"type":"object"}` (no `properties`/`required` keys) rather
  than `{"type":"object","properties":{},"required":[]}`. Equivalent.
- `additionalProperties` is not emitted (schemars default), matching the legacy `so(...)` form
  and the handlers' ignore-unknown-keys behaviour.

## Not yet migrated (tracked in `docs/stories/D-36-schemars-plugin-op-schemas.md`)

`gitlab`, `grafana`, `docker`, `huggingface`, `opsgenie`, `homer`✓, `asterisk`, `sql`,
`slack`, `websearch`, `jira`, `confluence`, `kubernetes`, `loki`, `prometheus`,
`alertmanager`, `aws`. Several use `flex_str`/`flex_i64` string-or-number coercion; their
handlers stay (schema-only struct, D-34 precedent) and any drift they surface will be recorded
here as they migrate.
