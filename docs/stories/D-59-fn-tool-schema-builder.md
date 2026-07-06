---
id: D-59
title: Closure-backed FnTool + runtime object-schema builder
pillar: Agent
status: done
epic: consumer-gaps
note: "from the 2026-07-06 downstream-consumer review: every flux tool is a bespoke impl Tool struct and ToolSpec offers only raw-Value or compile-time schemars schemas — consumers defining many tools dynamically re-invent a closure adapter + runtime schema DSL"
---

# FnTool + runtime object-schema builder

## Goal
Make "a tool is data" a first-class flux pattern: a closure-backed `impl Tool` (name, description,
JSON schema + async handler) plus a small runtime object-schema builder — so consumers (and flux
itself) can define families of tools programmatically instead of one bespoke struct each.

## Why (evidence)
- flux has ~40 hand-written `impl Tool for ...` structs (flux-tools toolchains/cargo/cognition/…);
  no closure adapter exists (`grep FnTool|from_fn|tool_fn` — none).
- `flux_spec::ToolSpec` offers `read_only(Value)` (hand-rolled raw JSON) and
  `read_only_typed<T: schemars::JsonSchema>` (compile-time types) — nothing for building an object
  schema from a field list at runtime (crates/flux-spec/src/lib.rs:116,135).
- The reviewed downstream consumer built both: a closure-tool wrapper (captured async handler +
  `Value` schema, backend errors folded to soft `ToolResult::error`) and a `req`/`opt`/
  `object_schema`/macro DSL — and re-rolled the closure shape a second time in another module. Only
  its error-mapping and client handle are app-specific.

## Acceptance
- [x] `FnTool` (or `tool_fn(...)` constructor) in flux-runtime: takes name/description/schema (+
      optional ToolSpec extras: group, effects, risk, permission subjects fn) and an async handler
      `Fn(Value) -> Future<Output = Result<Value, String>>` (exact shape per flux's Tool trait);
      handler errors fold to a soft `ToolResult::error`, never a hard failure. Object-safe enough to
      register in a `ToolRegistry` like any other tool.
- [x] Runtime schema builder in flux-spec: `object_schema([...])` with `req(name, type, desc)` /
      `opt(name, type, desc)` field helpers producing draft-07-compatible object schemas (same shape
      the existing tools' hand-rolled schemas use); composes with `ToolSpec`.
- [x] Dogfood: at least one existing simple in-repo tool (or a test tool used in flux tests) is
      expressed via `FnTool` to prove the adapter is sufficient — without churning the whole
      flux-tools catalog.
- [x] Failing-first tests: handler invoked with args; error → soft ToolResult::error; schema builder
      output shape (required array, properties, types, descriptions); registry registration +
      dispatch through the normal path.
- [x] Full gate green; consumer-compat `cargo check` clean (additive).

## Progress
- 2026-07-06 filed from the consumer review.
- 2026-07-07 implemented. **flux-spec**: new `schema.rs` module (`mod schema; pub use schema::{...}`
  in `lib.rs`), a `FieldType` enum (`String`/`Integer`/`Number`/`Boolean`/`Object`/`ArrayOfString`)
  plus `req(name, ty, desc)` / `opt(name, ty, desc)` field constructors and `object_schema(fields)` /
  `empty_schema()` builders — output is `{"type": "object", "properties": {...}, "required": [...]}`
  with only `req` fields listed in `required`, composing with `ToolSpec::read_only`. **flux-runtime**:
  new `fn_tool.rs` module (`mod fn_tool; pub use fn_tool::{FnTool, tool_fn};` in `lib.rs`) — `FnTool`
  wraps a captured `ToolSpec` + a boxed async handler `Fn(Value) -> Future<Output = Result<Value,
  String>>`; `Ok(value)` becomes the result content (a bare `Value::String` passes through unquoted,
  matching how most hand-written tools return plain text; anything else is its compact JSON encoding);
  `Err(message)` folds to a soft `ToolResult::error` — `execute` itself never returns `Err`. An
  optional `.with_permission_subjects(|params| ...)` builder overrides the trait's own empty default.
  `tool_fn(spec, handler)` is the ergonomic constructor, returning `Arc<dyn Tool>` directly for
  `ToolRegistry::register`. Dogfooded in flux-runtime's own test module: the `PingTool` fixture (used
  by the capability-scope tests) is now `fn ping_tool() -> Arc<dyn Tool>` built via `tool_fn` instead
  of a bespoke `impl Tool` struct — zero test churn beyond the two call sites, since a returned
  `Value::String("pong")` renders as the same plain `"pong"` content the old struct produced. Wrote
  failing-first tests for both crates; verified failing-first for `fn_tool.rs` by temporarily (a)
  making `value_to_content` always JSON-encode (broke the dogfooded ping-content assertion and the
  greet-handler test with a quoted-vs-plain mismatch) and (b) making handler `Err` propagate as a hard
  `Err` instead of folding (broke the soft-error test with a panic on `.unwrap()`), confirmed both
  failed for the right reason, then restored. Full gate green: `cargo build --workspace`, `cargo test
  --workspace` (0 FAILED across every crate, flux-spec 13/13, flux-runtime 60/60 incl. the new
  `fn_tool` module and the dogfooded ping test), `cargo clippy --workspace --all-targets -- -D
  warnings` (one type-complexity lint on the subjects-fn field, fixed with a `SubjectsFn` type alias),
  `cargo fmt --check` (root + `plugins/`, both clean). Consumer-compat `cargo check --workspace` in
  the downstream consumer's repo stays clean; it did refresh that repo's own `Cargo.lock` (a
  pre-existing flux workspace version bump 0.2.21→0.2.23 from unrelated prior work, picked up by
  running `cargo check` there) which was left in place rather than reverted, since the consumer repo
  already carried other unrelated uncommitted work. No new external dependencies were added to either
  crate (the boxed-future type uses only `std::future`/`std::pin`). Additive only — the sole
  pre-existing-file edits are `lib.rs`'s two-line module wiring in each crate plus the `PingTool` →
  `tool_fn` dogfood swap.

## Notes
- No macro export initially — the plain builder must stand on its own; a macro can follow if
  call-sites warrant it.
- Adoption story in the consumer's repo follows: rebase its closure-tool wrapper + schema DSL onto
  these.
