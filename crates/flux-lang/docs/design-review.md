# Design review — flux-lang evolution

> **Scope / status (read first).** This is the **original, pre-build** design review; its inline code
> observations (the `{"type":"object"}` placeholder, "optimizer/HIR are stubs", `OpSignature` carries
> "no types", "+3 nodes") describe the tree **at review time**. All five findings were resolved — see
> the **Resolutions** section below — and the build has since landed **P0–P6 + flux-app** (`OpSpec::lower`
> emits a real schema; `OpSignature` carries `param_types`; the optimizer and typed HIR are built; this
> design round's node delta was **+2** `ctx`/`ctx_append`, with P6b later adding the +5 Tier-1
> control-flow nodes). Live status is in [`STATUS.md`](STATUS.md); the later build-time review
> rounds (the Wave-1 dead-crate finding, review #2/#3) are tracked there and in
> [`evolution-impl-plan.md`](evolution-impl-plan.md).

**Reviewing:** [`docs/designs/flux-lang-evolution.md`](../../../docs/designs/flux-lang-evolution.md)
(+ [`evolution-impl-plan.md`](evolution-impl-plan.md) and [`STATUS.md`](STATUS.md)).
**Verdict:** sound direction, honest framing, strictly additive. Two seams need to be resolved on paper
before P2 code is written; the rest are scoping/trimming suggestions. Nothing here blocks the design.

This review was itself re-checked against the live tree (`ast.rs`, `runtime.rs`, `opspec.rs`, `host.rs`,
`analyze.rs`, `schema.rs`, the PRD). Where a claim is grounded in code it cites `file:line`.

---

## What is right

- **The language-vs-stdlib dividing line (§1.1) is the correct call.** Only `ctx`/`ctx_append`/`need`
  become nodes; the cognition verbs stay a registered op-pack riding the existing
  `call`/`bind`/`when`/`each` machinery. This honours the PRD §4 "deliberately small" non-goal while
  still elevating what the interpreter must actually understand.
- **Layering is carefully reasoned and internally consistent.** Pure ops in flux-tools (L2),
  model-backed `ai.*`/`synth` in a new provider-injected `flux-cognition` (L3) with each tool owning a
  `Box<dyn Provider>`, datasource verbs left at L5/L6, `ToolContext` untouched
  (`crates/flux-runtime/src/lib.rs:102-132` exposes a `spawner`, no provider), and the
  `flux-codegate` `layer()` entry called out. That is the clean answer.
- **"Where we are" (§2) and `STATUS.md` are honest.** Every Done row cites evidence; the ➕ markers
  separate "designed" from "built"; the doc openly flags that `OpSpec::lower()` ships a
  `{"type":"object"}` placeholder (`opspec.rs:46`) and that `await`/optimizer/HIR are stubs.
- **Additive framing de-risks the change.** +3 nodes on the existing 29, no `Value`/`TypeRef`/effect
  change, opt-in packs so a plain coding session sees no planner-prompt bloat, and every existing JSON
  flow still parses/analyzes/runs. The node-kind SSOT (`schema::node_kind_catalog`) + drift tests keep
  the docs in sync the same auditable way they are today.

---

## Must resolve before building P2

### 1. The budget-enforcement seam has no L0 trigger mechanism (§3.2)

The design says the budget "bites at model-op input assembly … when a model-op names a pack." But the
`ctx` node is interpreted in **L0 `runtime.rs`**, which is deliberately op-agnostic: it dispatches every
op through the injected `OpHost::dispatch(op, input)` (`host.rs:62`) and has no concept of "a model-op."
So the interpreter has nothing to branch on to decide *when* to dereference-and-shrink a `Ctx`.

The code makes this sharper than the prose admits:

- An op's signature, as the interpreter sees it, is `OpSignature` — and it carries only param **names**
  (`required_params` / `optional_params`, `opspec.rs:92-97`), **no types**. There is currently no way
  for the interpreter to know that `ai.reason`'s `ctx:` slot is `Ctx`-typed. Triggering the budget off
  the op-input schema therefore requires `OpSignature`/`OpCatalog::lookup` to start carrying per-param
  type info — a real change that the design does not mention.
- If instead the interpreter shrinks *any* `Var` that resolves to a `Ctx` value at arg-resolution time,
  that fires for **every** op, not just `ai.*` — contradicting "when a model-op names a pack."

The honest mechanism is probably "the op declares a `Ctx`-typed input in its schema, and budgeting keys
off that declaration." That makes the §3.4 op-input JSON Schema work (P1) a **hard prerequisite** for the
§3.2 budget (P2), not a sibling — and it makes type-carrying op signatures part of P1's scope. The spec
should state this trigger explicitly.

Secondary: §3.2 says shrink "by visibility tier then recency," but `Visibility` is a per-symbol property
of the **symbol table** (`ast.rs:128`), while a pack's members are value references. To shrink by
visibility the assembler must resolve each member back to its binding, coupling `Ctx` assembly to the
symbol table, not just the `ValueStore`. Worth calling out.

**Ask:** name the concrete budget trigger (schema-declared `Ctx` input vs. resolved-value type),
fold type-carrying op signatures into P1 if the former, and ship a v1 test that demonstrates a pack
actually shrinking — drops recorded in the run trace — so `ctx` does not land as a glorified struct
literal.

### 2. `need`-as-a-node is under-justified next to `ctx` (§3.3)

The `ctx` justification ("the interpreter must understand membership to assemble and budget a model-op's
input") is convincing. The `need` justification is not symmetric. §3.3 admits `need` just "produces a
`Need` value"; the loop is driven externally (`when $open.empty … retry max 2`); and `done_when` is
stored but **nothing in the design evaluates it** — P2's analyzer note only "validate[s] pack/need
references; `gaps` over `Need`." That makes `need` look like it could be a pure op (a `mkneed`) or a
plain `bind` of a struct, exactly symmetric with its complement `gaps` (which *is* a pure op). The
asymmetry — `need` a node, `gaps` an op — needs an explicit answer.

**Ask:** either give `need` a load-bearing reason to be a node (e.g. the interpreter drives the
`done_when` loop, which would also make `done_when` real), or demote it to an op and keep the node count
at +2 (`ctx`/`ctx_append`).

---

## Scoping / trimming suggestions (not blockers)

### 3. P1 mixes two risk classes

P1 bundles 14 prelude types + a new crate + pure ops + datasource naming + the foundational
`OpSpec::lower()` JSON-Schema rework. That last item touches **every** op and the planner catalog
(`schema_params` / `OpSignature::from_spec` consume the schema, `opspec.rs:62-113`), so it is a
different risk class from "register some named types." Consider front-loading op-input schema (and the
type-carrying signature work from §1) as its own P0, since both the prelude and the cognition pack
depend on it.

### 4. The 14-type prelude risks dead schema

Shipping `Span/Source/Chunk/Hypothesis/Decision/Verdict/…` before any op consumes them invites drift.
Consider trimming P1 to the types the v1 cognition pack actually touches
(`Claim/Evidence/Need/Ctx/Query/Answer/Blocked`) and growing the rest as ops need them — the
`prelude_type_catalog()` SSOT generator makes adding them later cheap.

### 5. The worked examples model the discouraged pattern (§3.5)

The examples call the generic `ai.extract`/`ai.reason` directly — exactly what §3.4 says to discourage in
favour of domain wrappers (`slots.extract`, `kb.rerank`). Showing one wrapper in an example would set the
intended norm instead of making the base pack look like the default.

---

## Summary

| # | Finding | Severity |
|---|---|---|
| 1 | Budget trigger has no L0 mechanism; `OpSignature` carries no types; schema work is a hard prereq, not a sibling | resolve before P2 |
| 2 | `need`-as-node under-justified vs `gaps`-as-op; `done_when` is dead | resolve before P2 |
| 3 | P1 mixes "register types" with the cross-cutting op-schema rework | scoping |
| 4 | 14-type prelude risks dead schema; trim to what v1 ops use | scoping |
| 5 | Examples use base `ai.*` instead of the preferred domain wrappers | minor |

The design is coherent and the phasing maps cleanly to the PRD. The two items to settle on paper before
writing P2 code are the L0 budget-trigger mechanism (#1) and the `need`-as-node justification (#2).

---

## Resolutions (applied to the evolution docs)

All five findings were folded into `../../../docs/designs/flux-lang-evolution.md` / `STATUS.md` /
`evolution-impl-plan.md`:

1. **Budget trigger** — resolved **without** the proposed type-carrying op signatures: the budget is now
   enforced **at `ctx`/`ctx_append` node evaluation** (the node resolves members, shrinks by
   visibility→recency to the declared budget, records drops, stores a bounded `Ctx`); consuming ops get
   the already-bounded pack at arg-resolution, so the interpreter stays op-agnostic. The `ValueStore`
   binding-metadata coupling is called out in P2, and a build→append→shrink test (drops recorded) is
   required. (§3.2, impl-plan P2)
2. **`need`-as-node** — **demoted to a pure op**, symmetric with `gaps`; the loop stays explicit
   (`repeat until`). Node count is now **+2** (`ctx`/`ctx_append`). (§3.3, §4)
3. **P1 risk classes** — op-input JSON Schema split into its own **P0** ahead of the prelude/pack.
   (§3.4, §8, impl-plan P0)
4. **Dead schema** — the v1 prelude is **trimmed** to `Span/Claim/Evidence/Need/Ctx/Query/Answer/Blocked`
   (+ `Patch/TestResult`); `Source/Chunk`, `Hypothesis`, `Decision/Verdict` grow on demand. (§3.1, impl-plan P1)
5. **Examples** — the research example now uses domain wrappers (`claims.extract`/`kb.synth`); the coding
   example stays on the base `ai.*` pack for contrast. (§3.5)
