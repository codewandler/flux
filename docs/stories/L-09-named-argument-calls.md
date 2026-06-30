---
id: L-09
title: Named-argument calls (deprecate positional binding)
pillar: Language
status: done
priority: 1
design: named-argument-calls
---

# Named-argument calls (deprecate positional binding)

## Goal

Make Flux-Lang `call` parameter **order non-load-bearing**: multi-param ops are called with a
single named object argument; single-param ops keep bare-value sugar. Deletes `x-param-order` and
makes `required` a set, which unblocks the schemars schema migration (story D-31) — `schemars`
doesn't emit `x-param-order`, so optional-param ordering is lost once schemas are derived.

Full design: [docs/designs/named-argument-calls.md](../designs/named-argument-calls.md).

## Acceptance

- [ ] **Failing-first test:** `analyze.rs` — a `call` with 2+ bare (non-object) args against a
  multi-param op produces a diagnostic ("use an object arg naming the parameters"). Test is added
  red, then goes green.
- [ ] A single bare value against a multi-param op is rejected at compile (ambiguous).
- [ ] A single bare value against a sole-param op still binds (parity with today).
- [ ] The lone-object passthrough still works unchanged.
- [ ] `schema_params` no longer reads `x-param-order`; `required` is treated as a set. The
  `optional_param_order_can_be_declared_explicitly` test is replaced/rewritten.
- [ ] The planner prompt instructs the model to use object args for multi-param ops; catalog
  examples updated. A mock-driven plan round-trips an object-arg `write`.
- [ ] `pipe`'s first-arg splice still works (contained exception, documented).
- [ ] All hand-authored `.flux` flows + examples migrated to object form for multi-param calls.
- [ ] `reference.md` + both `SKILL.md` files' `call` sections rewritten; generated tables regenerated
  (`UPDATE=1 cargo test -p flux-lang --test skill_in_sync` + `-p flux-flow --test skill_docs_in_sync`).
- [ ] Dev loop green: `cargo build/test --workspace`, `clippy -D warnings`, `fmt`, `flux-codegate`.
- [ ] CHANGELOG entry.

## Progress

- [x] Design + story written.
- [x] Implement `schema_params` set-semantics + drop `x-param-order`.
- [x] Analyzer: reject multi-bare-arg; type-check object form by field name.
- [x] Runtime: deprecated positional fallback; single-bare → sole required param.
- [x] Planner prompt + catalog examples.
- [x] Migrate hand-authored flows + examples.
- [x] Docs/skills rewrite + regenerate.
- [x] Gates green.

## Notes

- Blocked-on/then-unblocks: **D-31** (schemars migration). Do this first; D-31 then needs no
  ordering accommodation at all.
- `pipe` keeps its positional first-arg splice — a contained, documented exception (see design).
- Legacy stored plans: runtime keeps a deprecated positional fallback so they still execute; only
  the analyzer (new compilations) rejects the old form.
