# Design: flux-lang evolution — agent-cognition AST, language, and SDK

**Status:** **✅ Shipped (P0–P6 + flux-app)** — this remains the *design of record* (kept forward-looking),
but the phasing in §8 is built and live. The artifact **prelude**, `ctx`/`ctx_append` nodes, op-input
JSON Schema, typed HIR (`analyze::lower`), the **text parser** (`parse`/`format`), the **optimizer**
(`optimize` + `PhysicalPlan` execution), the **`flux-cognition`** (L3) pack, the **Program** layer +
**`flux-app`** (L6) host (`flux run app.flux`), and the **`flux-sdk` `FlowClient`** all exist. **P6** then
added **`await` cross-turn suspend/resume** (`run_top_level`/`resume_flow` + the engine `suspensions`
table), the **Tier-1 control-flow primitives** from §5.1 (`match`/`route`/`fallback`/`timeout`/`budget`),
and polish (`fluxlang compile`, the token-efficient `format_compact`, a deterministic thing resolver). Where
a section below says "deferred / not built / out of scope," read it as the gap *at design time* — live
status is in [`STATUS.md`](../../crates/flux-lang/docs/STATUS.md). · **Layers:** `flux-lang` (L0),
`flux-flow` (L3), `flux-cognition` (L3), `flux-app` (L6), `flux-sdk` (L6) · **Owner:** Timo Friedl

This is the forward design for **flux-lang as an agent *working* language**: the AST extensions, the
language/syntax surface, and a real SDK. It builds on the immutable PRD
([`../../crates/flux-lang/docs/PRD.md`](../../crates/flux-lang/docs/PRD.md) — the source-of-record, left
verbatim) and the integration design ([`flux-flow.md`](flux-flow.md)). Where the implementation stands
against the PRD is tracked separately in
[`crates/flux-lang/docs/STATUS.md`](../../crates/flux-lang/docs/STATUS.md). The implementation plan is
[`crates/flux-lang/docs/evolution-impl-plan.md`](../../crates/flux-lang/docs/evolution-impl-plan.md).

---

## 1. Thesis — symbols, values, and now *cognition*

The PRD's principle is unchanged:

> **LLMs operate over symbols. The runtime operates over values.**

The model is a compiler front-end that emits a typed, analyzer-validated execution graph; a
deterministic Rust runtime resolves symbols to stored values and runs registered, effect-typed
operations under one safety envelope. This design adds a vocabulary for the *cognition* of an agent
task — **needs, context packs, evidence, claims, hypotheses, patches** — so that the things an agent
actually manipulates are first-class, typed, and auditable rather than buried in prose.

In one sentence:

> flux-lang is a **typed, IO-free, context-first agent working language**. The LLM writes compact steps
> over symbols — building context packs, naming what it still needs, gathering evidence, proposing
> patches, retrying under bounds — and **every real-world effect happens through a registered,
> effect-typed op** under the existing `Executor::dispatch` envelope.

The non-goals from PRD §4 hold without exception: not a general-purpose language; external content is
**data, never orchestration authority**; only analyzer-validated AST executes; the model never
dereferences a value.

### 1.1 What this distills (and what it deliberately drops)

The starting point was a separate "AIML" sketch (an agent IR). Its **good ideas** — an artifact-type
ontology, first-class context packs, explicit needs, evidence-bearing claims, and `=`/`do`/`+=` marker
clarity — are distilled here because they *amplify* the PRD (its §13 *Context Management*, UC-A
*slot-filling* = needs, UC-B *KB/grounding* = evidence). Its **over-reach** — turning every cognition
verb (`extract`/`infer`/`judge`/`synth`) into language syntax — is dropped: those land as a registered
**op-pack**, keeping the language small per the PRD non-goal. The dividing line:

- **Language (new `Node` kinds):** only `ctx` / `ctx_append` / `need`. Justified because the runtime
  must *understand* them to enforce a context budget, compute the model-facing dependency slice, and
  drive a "what's still missing?" loop. An opaque op result cannot do that.
- **Standard library (types + ops, no new nodes):** the artifact ontology (a type *prelude*) and the
  cognition verbs (an *op-pack*). These ride the existing `call`/`bind`/`when`/`each` machinery.

---

## 2. Where we are (so the additions are honest)

Verified against the tree (full matrix in `STATUS.md`):

- **Shipped:** the typed AST (`crates/flux-lang/src/ast.rs`, **36 node kinds** — already well beyond the
  PRD's "deliberately small" v1 list), the reference interpreter (`runtime.rs`) over injected
  `OpHost`/`ValueStore`/`FlowSink`, the SQLite value/symbol store with visibility tiers
  (`flux-flow/src/state.rs`), the effects→policy bridge (`effects.rs`) on the one envelope, the NL→AST
  compiler (`flux-flow/src/compile.rs`), the ASCII renderer, and the schemars-driven node-kind SSOT
  (`schema.rs`) that generates the planner prompt + `reference.md` + both skills.
- **Partial / not started:** the **text parser** (`parse.rs`/`format.rs` are only a "Toolchain plan" in
  `syntax.md`), **HIR type/effect checking** (`HirFlow` is a stub; the analyzer does name/grammar/
  bounded-loop only), the **optimizer + `PhysicalPlan` execution** (the `Stage` types exist; nothing
  runs them), **op-input JSON Schema** (`OpSpec::lower()` ships `{"type":"object"}` at `opspec.rs:46`),
  **thing resolution**, the **UI graph projection**, and the **example domain packs**.
- **The SDK gap:** `crates/flux-sdk` (~190 lines) wraps the *classic agent loop*; it does **not** expose
  the flux-lang `compile → analyze → execute` lifecycle or op/type registration the PRD §17 specifies.

This design extends the shipped core additively and gives the partial/missing pieces a target shape.

---

## 3. Agent-cognition layer

### 3.1 Artifact-type ontology — a curated `prelude` (pure named types)

A standard library of the types an agent task manipulates, registered as `Named` type schemas through
the existing type machinery. **No `Value` enum change** (`Value` stays `Null/Bool/Number/String/Struct/
List/Thing/Ref` — `ast.rs:179`); every artifact is a `Struct` value with a `Named` `TypeRef`. The
prelude ships with the language; ops declare their inputs/outputs in these terms.

| Type | Shape (sketch) | Serves |
|---|---|---|
| `Span` | `{ source: Thing, range }` — a cited region inside a source | research, coding |
| `Source` / `Chunk` | a retrieved document / a slice of one | research, KB (UC-B) |
| `Claim` | `{ text, source: Thing, span: Span, confidence }` | research, KB |
| `Evidence` | `{ claim: Claim, support: [Span] }` | research, audit |
| `Need` | `{ ask, require: [field], done_when }` | slot-filling (UC-A), research |
| `Hypothesis` | `{ text, likelihood }` | coding, debugging |
| `Patch` | a concrete diff **or** a semantic edit (see §3.4) | coding agent |
| `TestResult` | `{ ok, failures, summary }` | coding agent |
| `Ctx` | a bounded context pack (see §3.2) | all (token efficiency) |
| `Decision` / `Verdict` | `{ choice, reasons, evidence }` | judge steps |
| `Query` | `{ find, near?, type?, sources?, after?, limit }` | datasource |
| `Answer` / `Blocked` | structured returns: `{ status, summary, evidence, gaps, risks }` | all |

**v1 scope (trim).** Ship only the subset the v1 cognition pack and examples actually touch — `Span`,
`Claim`, `Evidence`, `Need`, `Ctx`, `Query`, `Answer`/`Blocked`, plus the coding types `Patch`/`TestResult`
(`Span` is non-optional: `Claim`/`Evidence` reference it). The rest — `Source`/`Chunk`, `Hypothesis`,
`Decision`/`Verdict` — **grow on demand** as ops need them: shipping a type before any op consumes it
invites dead schema, and the `prelude_type_catalog()` SSOT makes adding one later cheap.

The external-item handle is the existing `Thing`/`ThingRef` (PRD §9.1) — the prelude adds **no** parallel
`Ref` type (`Value` already has a `Ref(ValueRef)` variant; a second `Ref` would collide). These
artifacts are **new**: they are *not* the same as `flux-evidence`'s `Observation`, which is a generic
audit bag (`{ kind, phase, data }`) feeding lifecycle reactions, not a typed claim with span/confidence
(`crates/flux-evidence/src/lib.rs:22-64`). The only honest link is one-directional: a produced
`Evidence` value may be **recorded into** the `EvidenceLog` as an `Observation`. The prelude stays
L0-pure (only `flux-core`/`flux-spec`/`flux-evidence`).

### 3.2 First-class context packs — `ctx` / `ctx_append` (the load-bearing elevation)

PRD §13 already makes the runtime compute a **policy-filtered symbolic projection** and a **dependency
slice per step** — but today that is implicit and global. This elevates it to an artifact the *program*
names, budgets, and shrinks. A context pack is "an intentionally bounded bundle of information."

**New node `ctx`** — build/declare a pack:

```jsonc
{ "kind": "ctx", "name": "auth_debug",
  "purpose": "explain failing refresh-token tests",
  "include": ["$src", "$fails", "$claims_high_conf"],
  "exclude": ["$generated"],
  "budget":  9000 }                       // token cap; runtime-enforced
```

Produces a `Ctx` value bound to `$auth_debug`. **Where the budget bites — at node evaluation, not at op
dispatch.** The interpreter is **op-agnostic**: it dispatches every op through `OpHost::dispatch(op,
input)` (`host.rs:62`) and op signatures carry only param *names*, no types (`opspec.rs:85-98`), so it has
nothing to branch on to decide *when* a `Ctx` should shrink — "budget at model-op input assembly" is
unimplementable in L0. Instead, the **`ctx`/`ctx_append` node itself enforces the budget, eagerly**: when
the node runs it resolves its members through the store, shrinks deterministically by visibility tier then
recency until within the declared `budget`, records the drops in the run trace, and stores an
already-**bounded** `Ctx`. Consuming ops then just receive the bounded members (inlined at arg-resolution
— op-agnostic; fires for any op that takes the pack). This needs **no type-carrying op signatures** for
the budget. The shrink reads per-symbol visibility/recency, so it couples `Ctx` assembly to the **symbol
table** (binding metadata), not just raw values — P2 may add a small `ValueStore` accessor for that. The
budget counter is **heuristic (char-based) in v1** (the existing `compact_threshold_chars` style); exact
token counting is later — no provider tokenizer belongs in L0.

**New node `ctx_append`** — the `+=` marker; accrete into a pack:

```jsonc
{ "kind": "ctx_append", "ctx": "auth_debug", "add": ["$more_src"] }
```

Immutable: it rebinds `$auth_debug` to a *new* `Ctx` value (preserving the audit chain
`$auth_debug@1 → @2`, exactly as the PRD's value-revision model already does for `$draft`) and
re-enforces the budget.

**Why these are language, not ops:** the node does real work an opaque op couldn't — it resolves a pack's
membership against the store, **shrinks it to budget at evaluation** (above), records the drops, and keeps
`ctx_append` an immutable rebind in the audit chain. A `mkctx` op returning an opaque blob would have
neither the store access nor the eager-budget semantics. This is a deliberate forward elevation of PRD §13
(explicit context management). Effects: both are `Pure` (they select/label existing values; no IO).

### 3.3 Needs & gaps — two pure ops (no new node)

Agents fail when missing information is implicit. A `Need` makes the gap explicit — `{ ask, require,
done_when }`. But `need` does **not** earn a node kind: it only constructs a value, the loop is ordinary
control flow, and there is nothing for the interpreter to *understand* (unlike `ctx`, which must shrink to
budget at eval). So `need` and its complement `gaps` are **both pure ops**, symmetric:

```jsonc
// need(...) -> Need        builds the artifact (Pure)
{ "kind": "bind", "name": "root_cause",
  "value": { "kind": "call", "op": "need", "args": [ { "kind": "lit", "value": {
    "ask": "Why are auth tests failing?",
    "require": ["failing_test", "relevant_code", "recent_change"],
    "done_when": "confidence >= 0.8" } } ] } }

// gaps(claims, need) -> [unmet field]   (Pure)
{ "kind": "bind", "name": "open",
  "value": { "kind": "call", "op": "gaps",
             "args": [ { "kind": "var", "name": "claims" }, { "kind": "var", "name": "root_cause" } ] } }
```

The loop stays **explicit** with the existing nodes — exactly the PRD UC-A slot-filling shape, no new
machinery:

```text
repeat max 3 until $open.empty
  ... gather more evidence ...
  $open = gaps $claims, $root_cause
```

`done_when` is a plain field on the `Need` that the explicit loop (or a domain wrapper) checks — nothing
in the interpreter evaluates it. Net: `need`/`gaps` are pure ops in the cognition pack; the only new
**nodes** are `ctx`/`ctx_append` (**+2**).

### 3.4 Cognition op-pack (registered ops — interpreter unchanged)

The cognition verbs land as a standard, opt-in op-pack so the language stays small. Names express
domain meaning (PRD §11):

- **Model-backed (`!Model`):** `ai.extract { from, schema, ask }` (e.g. `Claim[]`),
  `ai.rank { items, by }`, `ai.judge { claim, evidence } -> Verdict`, `ai.reason { ctx, ask }`,
  `synth { claims, format, cite } -> Answer`, `ai.rewrite { text, style }`.
- **Pure:** `need`, `gaps`, `compare`, `dedupe`, `sort`, `top`, `merge`, `cite`.
- **Datasource (`!Read`/`!Network`):** `query` (→ `Query`), and `Repo.search` / `Read.many` /
  `Test.run` / `Repo.patch`, which **lower onto existing flux-tools / flux-datasource ops** — they are
  naming conventions over the live `ToolRegistry`, not new machinery.

**Where each op lives (layering).** Split by whether a provider is needed: the **pure** ops
(`gaps`/`compare`/`dedupe`/`sort`/`top`/`merge`/`cite`) register in **flux-tools (L2)** — no provider, no
IO. The **model-backed** ops (`ai.*`, `synth`) go in a **new provider-injected pack `flux-cognition`
(L3)** — `CognitionPack::new(provider).register(&mut registry)`, each tool owning a `Box<dyn Provider>`;
**`ToolContext` is untouched** (it exposes a `spawner` but no provider — `crates/flux-runtime/src/lib.rs:102-132`).
Classify `"flux-cognition" => 3` in `flux-codegate`'s `layer()` map when the crate lands. The
**datasource** verbs (`query`, `Repo.search`, `Read.many`, `Search.run`) are the *existing*
`flux-datasource` (L5) / `flux-tools` ops surfaced at **L6** — **not** bundled into the L3 cognition
crate (that would invert the layering).

**The cognition pack is additive — `task` stays.** A direct single-shot model call (the `flux-cognition`
pack) and the existing **`task`** op (full sub-agent delegation via the `spawner`) are complementary, not
a replacement: `task` does delegated multi-step work; the cognition ops do one structured call. Both
remain. (Forward direction: some IO/LLM ops may later be promoted to *language primitives* — **not in
v1**; for now they are registered ops.)

**PRD §11 tension, resolved.** The PRD says *avoid making everything generic under `ai.*`*. So the
generic verbs are a **base pack**, and **domain wrappers are encouraged and preferred** in real apps:
`kb.rerank` = `ai.rank` with a fixed `by`; `slots.extract` = `ai.extract` with a slot schema;
`policy.check_email` stays a domain `!pure` op. The base pack exists so a new domain doesn't start from
zero; the wrappers exist so op names carry meaning.

**Op-input JSON Schema is a prerequisite (now P0).** `ai.extract`'s `schema:` argument, the planner
catalog, and the prelude types all depend on real op-input schemas — but `OpSpec::lower()` ships a
`{"type":"object"}` placeholder (`opspec.rs:46`) and `OpSignature` carries names-only params
(`opspec.rs:85-98`). That rework touches **every** op, so it is front-loaded as its own **P0** (see §8),
not bundled with registering types.

### 3.5 Worked examples (JSON-authored today; text shown for readability)

**Coding agent** (on the base `ai.*` pack, for contrast) — find → context → reason → patch → verify, with
evidence on the return:

```text
goal "fix: refresh tokens rejected after rotation change"

$h     = do Repo.search "refresh token rejected"
$src   = do Repo.read $h.top(8)
$facts = do ai.extract { from: $src, schema: Claim[], ask: "auth invariants, cite spans" }
$t     = do Test.run "pytest tests/auth -q"

ctx debug:
  purpose "smallest likely bug"
  include $src, $t.failures, $facts.high_conf
  budget 9000

$bug   = do ai.reason { ctx: debug, ask: "root cause + minimal fix" }
$patch = edit $src with $bug scope minimal preserve public_api
$r     = do Repo.patch $patch
$t2    = do Test.run "pytest tests/auth -q"

when $t2.ok
  return Answer { status: #fixed, patch: $r, tests: $t2, evidence: $facts.used }
else
  return Blocked { status: #needs_work, tests: $t2, gaps: $t2.failures.summary }
```

**Research / datasource** — gather → extract claims → close gaps → synthesize a cited answer. This one
uses **domain wrappers** (`claims.extract`, `kb.synth`) — the preferred pattern from §3.4 — and shows
`need`/`gaps` as plain ops driving an **explicit** loop:

```text
goal "what changed in enterprise pricing?"

$need   = need(ask: "concrete pricing changes since 2026-01-01",
               require: ["date", "plan", "price", "source"])   # need(): a pure op -> Need

$q      = query: find "enterprise pricing update" sources ["docs","tickets","slack"] after 2026-01-01 limit 30
$refs   = do Search.run $q
$docs   = do Read.many $refs.top(12)
$claims = do claims.extract $docs        # domain wrapper = ai.extract with a Claim[] schema
$open   = gaps $claims, $need            # gaps(): a pure op -> [unmet field]

repeat max 2 until $open.empty
  $more   = do Read.many (do Search.run (query: find $open.terms limit 10)).top(6)
  ctx += $more                           # ctx_append onto the working pack
  $claims = do claims.extract $more
  $open   = gaps $claims, $need

return do kb.synth $claims               # domain wrapper = synth with citation defaults
```

---

## 4. AST & docs-sync impact (concrete)

- **New `Node` variants:** `ctx`, `ctx_append` — **2** added to the 29 existing
  (`Assert/Await/Bind/Call/Confirm/Debounce/Each/Expr/Fmt/Jq/Lit/Loop/Memo/Parallel/Parse/Peek/Pipe/
  Race/Repeat/Retry/Return/Seq/Thing/Throttle/Try/Unless/Var/Verify/When`). (`need`/`gaps` are pure ops,
  not nodes.) Each gets a doc-comment;
  the node-kind SSOT (`schema::node_kind_catalog`) then regenerates the planner prompt, the
  `reference.md` table, and both skills. Regenerate with `UPDATE=1 cargo test -p flux-lang --test
  skill_in_sync` and `UPDATE=1 cargo test -p flux-flow --test skill_docs_in_sync`; keep both green.
- **No new effects, no `Value`/`TypeRef` change.** The `ctx`/`ctx_append` nodes and the `need`/`gaps` ops
  are `Pure`; model ops are `Model`; `query` is `Read`/`Network`. Artifact types are `Named` schemas.
- **New prelude-type SSOT.** Mirror the node-kind generator with a `prelude_type_catalog()` +
  `prelude_schema()` and a drift test, so the artifact ontology is generated into `reference.md` and the
  skills the same auditable way node kinds are. Add `ops-reference.md` rows for the cognition op-pack.
- **Frozen:** the 29 existing nodes, `DraftAst`/`HirFlow`/`PhysicalPlan` shapes, and the
  `OpHost`/`ValueStore`/`FlowSink` seams. The change is strictly additive.
- **Backward-compatible:** because the change is additive, every existing JSON flow still parses,
  analyzes, and runs unchanged — no AST schema migration is needed.
- **Planner-prompt cost is bounded:** +2 node kinds is a small grammar delta, and the prelude types +
  the cognition op-pack are **opt-in** (registered via `register_prelude`/`register_pack`), so a plain
  coding-agent session that doesn't register them sees no prompt bloat.

---

## 5. Language / syntax surface

The **wire format stays JSON** (the v1 authoring path; see `examples/eval-smoke.flux`). This section
designs the *human-readable* form so the deferred parser (`parse.rs`/`format.rs`, PRD roadmap items 1–2)
has a target; it is **not** built this round.

- **Three markers make the boundary visible:** `=` a pure transform / bind · `do Op.op args` an
  effectful external op (so effects are legible at a glance — aligns with the PRD's effect-first
  stance) · `+=` a context append (`ctx_append`).
- **Block forms:** `ctx <name>:` , `need <name>:` , `query …` — indentation-delimited attribute blocks,
  consistent with the existing `key: value` convention in `syntax.md`.
- **Optional `goal "…"` header** on a flow/program — an objective string that improves the audit trail
  and seeds the compiler prompt (cheap, additive to the flow header).
- **Two display modes** (PRD item 1): a human-readable form (above) and a token-efficient low-level form
  for model context (a candidate for a future fine-tuned emitter). Both round-trip to the same AST.
- **Opt-in surface:** the `ctx`/`need`/`query`/`goal` forms only appear in files that use them; a bare
  coding flow reads exactly as today. These markers are a *display* concern — the JSON wire format and
  the v1 authoring path are unaffected.

### 5.1 Candidate control-flow primitives (Tier-1 adopted in P6b)

> **Status:** the **Tier-1** set below — `match`, `route`, `fallback`, `timeout`, `budget` — is **built**
> (P6b; see `STATUS.md`). **Tier-2** (`checkpoint`/`compensate`/`once`/`scope`) remains proposed.

Flux-Lang already has a broad control set (`when`/`unless`, `repeat`/`each`/`loop`, `seq`/`pipe`,
`parallel`/`race`, `try`/`retry`/`confirm`, `assert`/`verify`, `return`, `await`, `memo`/`throttle`/
`debounce`). These are **proposals** — none implemented yet — for steering execution more intuitively
while holding the thesis: *deterministic, guard-railed, minimal non-determinism*. Each is tagged **node**
(a control construct) or **op** (a pure helper), with its effect on determinism. Any adopted node goes
through the node-kind SSOT + docs-sync gates (AGENTS.md).

**Tier 1 — steer flow, directly on-thesis:**

- **`match` / `switch`** (node, deterministic) — a multi-way **exhaustive** branch over a value or union
  type; replaces chains of `when` (e.g. the `call-routing` intent dispatch). Exhaustiveness checking is
  itself a guard-rail.
- **`route` / `select`** (node, *bounded* non-determinism) — the signature flux primitive: the **selector
  is a `!model` op**, but the **cases are fixed and analyzer-validated**. The model chooses *which*
  declared branch runs, never *what* runs — a typed router. This is exactly "minimal, contained
  non-determinism," and it serves UC-A/UC-B and `call-routing` directly.
- **`fallback` / `or_else`** (node, deterministic) — an ordered "first that succeeds / is non-empty wins"
  selector (cheap path → else expensive path). The one genuinely useful behaviour-tree idea (selector
  semantics), as a structured try-chain — lighter than `try`/`catch`. Steers graceful degradation.
- **`timeout` / `deadline`** (decorator node, deterministic) — bound the wall-clock of any sub-flow
  (generalizes `race`'s `timeout` and `loop`'s `for_ms`). A reliability guard-rail you can wrap around
  anything.
- **`budget` / `limit`** (scope node, deterministic) — cap **cost** within a scope: tokens, model calls,
  or money (`budget tokens 10k { … }`). Ties to the `ctx` budget (§3.2) and `throttle`; a first-class
  cost guard-rail.

**Tier 2 — durability & side-effect safety (workflow-engine depth; add on demand):**

- **`checkpoint`** (node) — a durable resume point so a re-run continues from the last completed step
  (pairs with `await`/`memo`); the backbone of long-running/resumable flows.
- **`compensate` / saga** (node) — register a compensating action for a completed side-effect; if a later
  step fails, the runtime unwinds by running compensations in reverse. The strongest "guard-railed side
  effects" tool for non-transactional external systems.
- **`once` / idempotency-key** (decorator) — a side-effect runs at most once across re-runs (an
  effect-level `memo`). Safety under re-execution.
- **`scope` / `with`** (node) — acquire → use → release (a lock, a transaction, a temp resource) with
  guaranteed cleanup on early `return` or error. RAII for flows.

**Better as pure ops, not nodes** (keep the language small): `default`/`coalesce` (null-fallback) and the
collection transforms `map`/`filter`/`reduce`/`sort`/`dedupe`/`top` (these overlap the pure cognition
verbs in §3.4).

Net effect on the thesis: only `route`/`select`'s selector (and anything wrapping a `!model` op) is
non-deterministic; every other primitive above is **deterministic control** — more steering power, the
same guard-rails.

---

## 6. Multi-agent program layer (language half; runtime host deferred)

The broader "one `.flux` file describes a whole multi-agent app" vision is expressed *from the flux-lang
perspective* here; its long-running executor is deferred to the appendix.

A file may be a **`Program`** (a module), not just one flow:

```
Program { types, agents, channels, conditions, triggers, journeys, flows }
```

- **Agents/channels/triggers** are pure-data declarations (identity, model/tools/datasource access;
  input/output surfaces; event→action bindings). `AgentDecl` is a superset of the existing
  `flux_orchestrate::role::Role`.
- **Journeys are flows.** A `JourneyDecl` embeds a `DraftAst`; it runs on the existing interpreter
  unchanged.
- **Orchestration is an op-pack** (`ask` / `send` / `emit` / `spawn`) — agent/channel interaction is op
  dispatch, so this layer needs **zero new node kinds**. "User input is just an event": a trigger's `on`
  label shares the event-label space with `Node::Await`.
- **Back-compat:** a bare single-flow file still parses (a key-sniffing loader wraps a lone `DraftAst`).

Decls stay L0-pure (strings + a `settings` JSON map); the L3 engine owns model/datasource/channel
*meaning*. This keeps the multi-agent vision coherent without expanding the language core.

---

## 7. The SDK (`flux-sdk`) — the PRD §17 lifecycle, finally exposed

The current `ClientBuilder → Client.run` (agent-loop wrapper) is **kept as the simple front door**. The
SDK grows a **flux-lang lifecycle surface** that reuses flux-flow's adapters (it does **not**
re-implement the envelope):

- **Registration:** `OpRegistry` + `register_op` / `register_pack` (cognition, datasource, orchestration
  packs) + `register_prelude` (artifact types). One call yields batteries-included cognition.
- **Lifecycle (PRD §17):** `compile_turn(text, view, registry, llm) -> DraftAst` (re-expose
  `flux-flow`) · `analyze(ast, session, registry, policy) -> HirFlow` (richer over time) ·
  `optimize(hir) -> PhysicalPlan` (future) · `execute(ast|plan, session, host/store/sink) ->
  ExecutionResult` (`execute_flow` today).
- **Artifact ergonomics:** typed Rust builders/readers for `Ctx`/`Need`/`Claim`/`Evidence`/`Patch`/
  `TestResult`, plus result readers for *evidence used / gaps open / risks* — the API the PRD
  "Developer" persona needs to embed flux-lang.
- **`FlowClient`:** a high-level façade tying provider + op-packs + compile→analyze→execute and returning
  structured artifacts — the recommended entry point for building AI apps on flux-lang, and the thing
  the roadmap's "SDK + crates.io" tier publishes.

---

## 8. Phasing (maps to PRD milestones; detail in the impl plan)

| Phase | Scope | PRD link |
|---|---|---|
| **P0** | **op-input JSON Schema** — `OpSpec::lower()` emits real schemas; cross-cutting (every op + planner catalog). Prerequisite for P1/P2. | item 4, §11 |
| **P1** | v1-core prelude types + cognition op-pack (incl. `need`/`gaps` as pure ops) | §11 |
| **P2** | `ctx` + `ctx_append` nodes + **budget enforced at node-eval** + SSOT/docs sync | §13 |
| **P3** | SDK lifecycle surface (`OpRegistry`/packs/prelude + `FlowClient` + artifact APIs) | §17 |
| **P4** | richer `analyze`: type + effect checking, `DraftAst → HirFlow` | item 3, §10.2 |
| **P5** | text display modes + parser (items 1–2) and/or optimizer + `PhysicalPlan` exec (M5); the **Program/multi-agent layer + `flux-app` runtime host** run as a parallel track | items 1–2, §15 |

Every phase ships behind the full dev loop (`build`/`test`/`clippy -D warnings`/`fmt`/`flux-codegate`)
with a test that fails before the change.

---

## Appendix — runtime host (`flux-app`) — ✅ shipped

> **Status: shipped.** Implemented as the L6 **`flux-app`** crate: an event bus (`bus.rs`), channels
> (`channel.rs` — the `cli` channel today), journeys executed under the real `Executor` envelope
> (`app.rs`), and orchestration ops `emit`/`send`/`spawn` (+ `ask` MVP) as an op-pack (`ops.rs`). Run via
> `flux run app.flux`; **deny-destructive by default** (`--yes` opts into allow-all). The scheduler and
> HTTP/Slack channels remain future work. The "does not exist yet" framing below is the original design
> rationale, kept for context.

Executing a `Program` long-running (the "phone-troubleshooting" story) needs infrastructure that does
**not** exist yet: an in-process **event bus** (tokio broadcast + oneshot correlation), a **scheduler**
(interval/at; `cron` only on demand), a **channel runtime** (CLI/HTTP/Slack listeners reusing
`flux-server` auth + `flux-integrations` parsing), **agent instances** (long-lived `FlowEngine`s with
scoped `Executor`s), the orchestration ops as `Tool`s through `Executor::dispatch`, and a **supervisor**
loop (cancellation/shutdown). It would be a new **L6 crate `flux-app`** (registered in
`flux-codegate`'s `layer()` map), driven by `flux run app.flux`. The safe headless default is
deny-destructive (the `SubAgentApprover` policy) plus a human approval channel — never allow-all. This
is captured so the language design above stays consistent with the eventual executor; it is **out of
scope for the current round**.
