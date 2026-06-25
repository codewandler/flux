# Design: flux-flow (Flux-Lang)

**Status:** Draft design · **Layer:** new L3 crate `flux-flow` · **Owner:** Timo Friedl

Canonical design reference for **Flux-Lang**, delivered as the `flux-flow` crate.
[architecture.md](../architecture.md) is the existing system design; [vision.md](../vision.md) is the
*why*. The implementation plan is a local working doc under `.flux/` (gitignored, not committed):
`.flux/plans/flux-flow-implementation.md`.

## 1. Thesis — the LLM is not the runtime

> The LLM plans. The runtime runs.

Every mainstream agent makes the LLM the **runtime scheduler**: it decides each step live, re-reads the
previous tool output every turn, and the whole transcript is re-sent so it can choose the next move.
That is slow, expensive, non-deterministic, and injectable. flux inverts it: the LLM is a **compiler
front-end** that turns an instruction into a typed, **readable execution graph** (an AST); a
deterministic Rust runtime resolves *symbols* to stored *values* and executes registered *operations*
under policy, with risk-gated approval, and can **re-run the graph later with the fewest possible model
calls**.

The payoff is a list, not a slogan: **token savings, security, speed, repeatability, consistency,
reliability, determinism, and an audit trail you read like Go or Rust** — not a black box.

This is flux's existing safety thesis pushed up one layer. The envelope
(`flux_runtime::Executor::dispatch`) already makes *tool execution* non-bypassable; Flux-Lang makes
*orchestration* analyzer-validated and effect-typed, reusing that exact envelope as its execution
substrate. flux already owns most of the machinery — the policy engine (`flux-policy`), the
operation/registry model (`flux_runtime::Tool` + `flux_spec::ToolSpec`), event-sourced sessions
(`flux-session`), and an effects→policy bridge (`effect_requests`). What is new is the language layer
(AST/HIR/plan), a typed symbol table + value store, things/resolution, and graph rendering.

## 2. What flux is now

flux is a **deterministic execution engine for engineering work**, shipped as three things from one core:

- **The SDK is a product.** `flux-sdk` exposes the lifecycle — compile a natural-language instruction
  into a flow, optionally render/verify it, run it (or persist it), and re-run it later with bound
  parameters. Others build AI applications on this.
- **The coding agent (CLI/TUI) is the flagship app.** The emotional north-star is engineers choosing the
  flux coding agent over Codex & co. Coding is now *one app on the core*, not the center of gravity.
- **Engineering operations are the broader market.** Incident response, live debugging, and ops across
  Slack / Grafana / kubectl / shell. Integrations arrive through **fluxplane / plugins / bash as
  registered ops** — flux-flow *composes* them into graphs, it does not rebuild them. (This is exactly
  why "ops lower to `Tool`/`ToolSpec`" is the right call: a kubectl call or a Slack post is already a
  flux tool/plugin; flux-flow orchestrates them deterministically.)

The dream loop: an engineer instructs the agent; the agent **plans** an execution graph; depending on
its **risk** the human approves it or not; it runs; and it can be **re-run later** with near-zero model
calls.

## 3. Two regimes, both first-class

One pipeline serves a spectrum, and the engine picks per instruction based on how much structure is
statically knowable:

| | **Incremental** | **Planned** |
|---|---|---|
| Shape | 1–2 ops, then the model re-enters to decide the next step | a branchy graph up front with `!model` decision-nodes + `when`/`repeat` |
| When | exploratory coding; live debugging where step N+1 depends on step N's *content* | ops runbooks, automations, anything statically knowable |
| Primary win | don't re-send raw outputs; symbol references; effect typing + audit | + parallelize / cache / approval-fence / **persist & re-run** |
| Latency | **must equal today's loop** — no extra compile round-trip | one compile amortized over many ops |

**Planned flows are central, not an edge case** — ops/incident work is far more plannable than pure
coding, and it is where determinism and re-run pay off. *Hybrid* is the general case: a coarse graph
that pauses and refines at decision points (model-in-the-loop). The single-op incremental path stays a
first-class, latency-preserving fast path so the coding agent never feels slower than today.

## 4. What Flux-Lang actually guarantees (claims, split honestly)

- **Token efficiency (true).** Values are stored once and referenced by symbol; raw outputs are not
  *re-streamed* across turns (today the full `result.content` lives in the message log forever —
  appended at `flux-agent/src/lib.rs:293`, reloaded at `:155`).
- **Injection resistance (qualified).** `!model` ops *do* feed scoped values to the model — that is how
  coding decides the next edit — so "values never reach the model" is **false** and is not claimed. The
  property that holds: **external content cannot rewrite control flow, policy, or op selection**, because
  the AST is analyzer-validated against the registry and policy before anything executes.
- **Determinism / re-run (configurable).** "Deterministic" describes the **orchestration** (control
  flow, data flow, effect ordering), not the values. Some flows need **zero LLM** (fully deterministic;
  `!model` outputs frozen); others have inherent non-determinism (a summary step). Both are supported,
  **configurable per op / per flow** (`cache` vs `live`). Re-run **always skips the NL→graph compile**.

## 5. Architecture

### 5.1 One crate, modules inside

`flux-flow` is a single crate at **layer L3** (it sits beside `flux-agent`). L3 is forced by two modules:
`compile` needs a `Provider` (L1) and `runtime` needs `flux-runtime`/`flux-system` (L2). Classify
`"flux-flow" => 3` in `flux-codegate`'s `layer()` map. Deps (own layer or lower): `flux-core`,
`flux-spec`, `flux-policy`, `flux-evidence`, `flux-secret` (L0); `flux-provider` (L1); `flux-runtime`,
`flux-system`, `flux-session`, `flux-context` (L2).

| Module | Role |
|---|---|
| `ast` | pure types: `DraftAst`, `HirFlow`, `PhysicalPlan`/`Stage`, `Value`, `Binding`, `ThingRef`, `TypeRef`, `FlowEffect`, `RunEvent`, ids |
| `parse` | parser + pretty-printer + JSON-AST schema |
| `analyze` | name/type/effect/bounded-loop checks → `HirFlow` or diagnostics (pure; no IO imports) |
| `optimize` | `HirFlow → PhysicalPlan`: dependency graph, parallel grouping, fence insertion, cache keys, report |
| `registry` | `OpSpec` (`fn lower() -> ToolSpec`), `OpRegistry` over `ToolRegistry`, `ThingResolver` + `ModelClient` traits |
| `state` | `SymbolTable` (visibility tiers), `ValueStore` (immutable, versioned, **budgeted**), `ThingStore`, `view(Session)`, **flow persistence** |
| `runtime` | interpreter (`bind/call/when/repeat/await/return`); thing-resolution at exec; emits `RunEvent` + bridging `Observation`; await/resume; re-run |
| `compile` | `compile_turn(NL, view, registry, llm) -> DraftAst`; schema-constrained output; analyze→repair loop; **determinism-biased prompt** |
| `engine` | turn spine: incremental fast-path vs. planned compile; execute; risk-gated approval; update session |
| `render` | pure projections of the graph + trace for CLI/TUI (ASCII DAG / indented tree) + per-node live status |

Op packs and resolver impls that need L5 capabilities (`flux-datasource`, `flux-browser`) and the
fluxplane/plugin op packs are registered **externally** (e.g. in `flux-cli`); L3 cannot depend on L5, and
ops are embedder-registered anyway. The L0-purity a crate wall would otherwise enforce for
`ast`/`parse`/`analyze`/`optimize` becomes a **module discipline** (no IO imports), guarded by a unit
test on those modules' import set.

### 5.2 The pipeline

**Incremental fast path (single op / chat):** the model emits one constrained tool-call inline in the
normal stream → `analyze` (cheap, local) → `Executor::dispatch` → store output as a `Value` + `Binding`
→ next turn the model sees a *summary* (not re-sent bytes), or content via a scoped `!model` op. **No
extra round-trip vs. today.**

**Planned path:**
```
user_input + view(Session)
  → compile::compile_turn   → DraftAst    (LLM, schema-constrained, determinism-biased; repair on invalid multi-node AST)
  → analyze                 → HirFlow | diagnostics
  → optimize                → PhysicalPlan (Sequential/Parallel/Branch/Repeat/Await/ApprovalFence)
  → [render graph; risk(graph); if risky → approve whole plan once]
  → runtime::execute(plan)  → Executor::dispatch per call node (Parallel via join_all; !model ops use the ModelClient seam)
                            → ValueStore writes + RunEvent trace + bridging Observations + live node status
  → project Session' + ONE assistant summary message into the message log
```

### 5.3 Operations lower to the existing envelope

A Flux-Lang `OpSpec` carries richer language metadata (typed `inputs`/`output`, `EffectSet`, `retry`,
`idempotency`, `approval`, `cache`) and **lowers to `flux_spec::ToolSpec`** via `fn lower()`. Each op is
an `impl flux_runtime::Tool`; the interpreter resolves symbols → concrete values *before* dispatch, so
**every op call — including `!model` ops — goes through `Executor::dispatch`**. No new bypass surface.

### 5.4 The `ModelClient` seam (gap fix)

`!model` ops must call a Provider, but today's `ToolContext` (`flux-runtime/src/lib.rs:69`) has no
Provider — and the op packs are built largely from `!model` ops. Fix: extend `ToolContext` with an
`Option<Arc<dyn ModelClient>>` (a narrow L1 trait over `Provider`; L1 < L2, no layering violation),
injected at executor construction, **fail-closed**. `!model` ops remain subject to the policy floor +
approval gate. The envelope is *not* strictly "untouched" — it gains a provider seam used only by
`!model` ops; that trade-off is deliberate.

### 5.5 Effects

The existing `flux_spec::Effect{Read,Write,Network,Process,Browser,Filesystem,LocalSystem}` is
host-resource-shaped (consumed by `effect_requests()`). Flux-Lang adds a **parallel semantic**
`FlowEffect{Pure,Read,Model,Network,WriteFile,WriteDb,SendExternal,Delete,Money,Calendar,HumanVisible}`
with a total mapping `FlowEffect → (Effect, Option<policy::Action>)`. Resource effects reuse the bridge;
semantic-only effects add policy `Action`s (`flow.send_external` / `flow.money` / `flow.calendar`) gated
by ordinary `Grant`s, with `Delete`/`Money` denied by default. The host-resource enum is not polluted.

### 5.6 Things — the full SWE+ops loop

"Excellent coding agent" means the *whole* engineering workflow: edit code **and** open/review PRs, file
& triage issues, post to Slack, trigger CI/deploy, schedule. So `Person`/`Ticket`/`PR`/`CalendarEvent`
resolution and `SendExternal`/`Calendar` effects are **core**, not ballast. `File`/`Repo`/`Url` resolve
directly through existing `Workspace`/path primitives (no ambiguity); the richer resolver
(`confidence`/`Source`/disambiguation) phases in. The rule "no side effects until required things are
resolved unambiguously" holds at execution time.

## 6. Flows as artifacts

A flow is first-class and durable: **NL instruction → compile to graph → optionally user-verifies → run
now, or persist and re-run later** with bound parameters. Persisted flows live under
**`.flux/flows/<name>`** (serialized AST + parameter schema + determinism knobs). Re-run loads the saved
graph and executes it, **skipping compilation**; `cache`d `!model` nodes replay frozen outputs (zero
model calls), `live` nodes re-invoke against current data. This is the repeatability/cost story: turn a
one-off instruction into a reusable, auditable runbook.

The SDK surface: `compile(nl) -> Flow`, `flow.render()`, `flow.risk()`, `flow.run(approver)`,
`flow.save(name)`, `load(name).run(params)`.

## 7. Approval, determinism, error handling

- **Approval is risk-gated and whole-plan.** `compile → risk(graph) → if risky: approve the entire
  resolved plan once (exact ops/args/effects + rendered graph) → run unattended`. Plan-approval is a
  **batch grant over exactly that plan's ops**. The per-op `Executor::dispatch` gate stays the floor; a
  destructive op the plan-grant didn't cover still escalates. **`--yes` auto-approves everything**, as
  today.
- **Determinism knobs.** Each `!model` node is `cache` (freeze, zero LLM on re-run) or `live`; pure/read
  ops cache by op-version + input-hash. The **compiler is explicitly prompted to prefer deterministic
  ops and minimize `!model` nodes** — fewer model nodes ⇒ cheaper, more repeatable flows. This prompt is
  a designed artifact with its own evals.
- **Error handling is a first-class concern, kept unrestricted in v1.** Operations **must** declare
  `retry`/`idempotency`/`approval`/`effects`; the runtime uses this to auto-retry/gate. Explicit
  language constructs (`try`/`on_error`/`escalate`/`retry max N`, human-in-the-loop) are **left open** —
  designed as the language matures; the AST is built to host them later. **Model-in-the-loop
  self-correction**: on failure a `!model` recovery op receives the error + context and proposes the next
  step, bounded (this is the incremental↔planned bridge).

## 8. Session model & value-store lifecycle

| Dimension | Storage | Source of truth for |
|---|---|---|
| Message log (exists) | `messages` table | provider history streamed to the LLM |
| Symbol / Value / RunEvent / Flows (new) | new tables, same SQLite DB | deterministic execution facts |

The message log stays the provider-history source of truth, preserving every **session-shape invariant**
(never an empty assistant message, a split tool_use/tool_result pair, or user-after-user). A planned
flow emits **one assistant summary per turn** (or one question on `await`); symbol/value/event state is
projected into context via a `flux-context` `ContextProvider`, never sent as messages — so the bug class
cannot reappear. `await` persists `RunEvent::Awaiting` + suspended state, emits one question, ends the
turn; the next user turn resumes the interpreter. This await/resume path is the "fourth sibling" of the
cancel/compaction/iteration-cap shape-fixes — validate via `scripts/smoke-live.sh` (the mock provider
does not catch shape violations).

**Value-store lifecycle.** Every op output is an immutable `Value`; over a long session this would grow
without bound — the compaction problem on a new surface. The design budgets it from the start: cap stored
bytes per session, evict oldest non-pinned, non-referenced values (externalize bytes to content-addressed
storage, keep the hash + trace ref so **eviction never breaks re-run**). *Storage* lifetime is distinct
from *visibility* tiers.

## 9. Graph rendering & auditability

Besides security, **auditability is a core principle**: the execution graph and the `RunEvent` trace must
read **as intuitively as Go or Rust**. v1 renders the parsed graph in CLI/TUI (ASCII DAG or indented
tree) and lights up per-node status during execution — which doubles as the **progress UX** for planned
flows (you watch nodes execute instead of streaming prose). No hidden control flow: every branch, effect,
and approval is visible. Faithful *audit-replay* (reconstruct a past run from recorded outputs) is
postponed; *re-run against live data* is the v1 feature. The **visual editor** is deferred to an IDE/web
move.

## 10. Safety: the envelope stays the single authority

- Every op (incl. `!model`) routes through `Executor::dispatch` — hooks → policy floor → permission
  rules → evidence/escalation → approval gate → guarded IO.
- **`ApprovalFence` is a scheduling marker, not the enforcer.** The optimizer inserts a fence before any
  node whose `OpSpec` would trip the gate; allow/deny still happens in `dispatch`. A regression test
  strips the fence and asserts a destructive op *still* prompts — the optimizer is not a security
  boundary.
- The `flux-evidence` log remains the security/audit trail and the sole input to `DestructiveEscalation`;
  `RunEvent` is the complementary execution trace.

## 11. Relationship to the free-form loop & cutover

The free-form loop (`flux-agent::run_turn_cancellable`) stays the **default and fallback**, behind a
`FLUX_LANG` flag, and is **later re-expressed as a trivial Flux-Lang flow** once the language covers its
semantics — then retired (a deletion, not a refactor, because the shared message-log shape invariants are
extracted to an L2 helper). The **default cutover is gated on coding *and* ops parity dogfooding**: a
fixed head-to-head suite (multi-file edits, read→fix loops, an incident runbook, a Slack/kubectl flow)
must show no regression in success rate, turn count, and p95 latency. `docs/vision.md` is updated to the
new claim *before* the default flips.

## 12. Resolved decisions & deferred details

Resolved in design (no longer open):

- **Compact syntax.** The JSON AST is the canonical, persisted form; compact syntax is a *readable
  review projection* (rendered in CLI/TUI for auditability). A full public authoring grammar waits for
  the editor.
- **Policy expression.** Reuse `flux-policy` `Grant`s with the new semantic `Action`s
  (`flow.send_external` / `flow.money` / `flow.calendar`); no new policy language in v1.
- **`cache` vs `live` default.** The compiler prefers deterministic ops and minimizes `!model` nodes; a
  `!model` node defaults to `live` (fresh, safe) and opts into `cache` when a flow is persisted for
  repeatable re-run. Overridable per op / per flow.
- **Value eviction.** Referenced or pinned values are never evicted; only unreferenced, non-pinned
  values are evicted (bytes externalized to content-addressed storage, hash + trace ref retained). A
  referenced value never hard-fails on re-run.

Deferred to implementation (detail, not concept):

- Flow parameter schema + on-disk format under `.flux/flows/<name>` — specified at M6.
- Exact `view(Session)` projection format and its token-budget interaction with compaction — M1/M3.
