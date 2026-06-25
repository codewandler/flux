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
| `compile` | `compile_turn(NL, view, registry, llm) -> DraftAst`; **prompt-and-parse** (no forced structured output); analyze→repair loop; **determinism-biased prompt** |
| `engine` | turn spine: incremental fast-path vs. planned compile; execute; risk-gated approval; update session |
| `render` | pure projections of the graph + trace for CLI/TUI (ASCII DAG / indented tree) + per-node live status |

Op packs and resolver impls that need L5 capabilities (`flux-datasource`, `flux-browser`) and the
fluxplane/plugin op packs are registered **externally** (e.g. in `flux-cli`); L3 cannot depend on L5, and
ops are embedder-registered anyway. The L0-purity a crate wall would otherwise enforce for
`ast`/`parse`/`analyze`/`optimize` becomes a **module discipline** (no IO imports), guarded by a unit
test on those modules' import set.

### 5.2 The pipeline (one loop)

Every turn runs the **same** loop; a single op is the degenerate case of a plan:
```
user_input + conversation + view(Session)
  → compile_turn            → TurnOutput::Plan(DraftAst) | TurnOutput::Chat(text)
                              (LLM prompt-and-parse; PURE DAG — the model's only tool is emit_plan;
                               analyze→repair on invalid AST; emit_plan, or answer in prose)
  → [Chat]  persist ONE assistant message; turn ends
  → [Plan]  render the plan (auditable) → analyze → risk(graph) → execute_flow:
              Executor::dispatch per call node; bind/call/return + when/repeat control flow;
              per-op approval at dispatch (destructive escalates); !model ops use the ModelClient seam
            → ValueStore writes + RunEvent trace + live node status (sink)
  → emit_plan carried a `reply` (plan completes the task) → show it, turn ends (ONE round)
  → else feed the plan's result back *ephemerally* and loop (read→reason) until reply/prose
  → persist Session' + ONE assistant summary into the message log
```
**Pure DAG:** the model has no directly-callable ops — *every* operation, reads included, is a node in
the emitted plan. To gather context it emits a plan with read nodes; the runtime executes it and feeds
the result back so it can plan the next step (the loop above). The persisted log stays
`user → assistant(text)` — symbols + summaries carry state forward, raw outputs are never re-sent.
(`Parallel` scheduling + the optimizer are M6.)

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

### 5.7 Two-face tool results & the file-tool surface

A `flux_runtime::ToolResult` carries **two faces**: `content` (the *canonical* value — bound to a symbol,
spliced into `{{symbol}}` interpolation, and used for `when`/`return` truthiness) and an optional `view`
(the *LLM-facing* rendering shown to the model and user). `runtime::execute_call` binds/stores the
canonical `content`; the model-facing observation (and the per-op sink line) use `view()` — so a `read`
stores the raw file bytes (clean to interpolate) while showing the model a line-numbered view, and an
`edit`/`write` attaches a unified diff without polluting the bound value. `view` defaults to `content`.

On that base, the built-in file tools (`flux-tools`) match a strong coding agent:
- **`read`** — canonical = raw bytes; view = line-numbered. Detects binary (NUL sniff) and refuses; an
  unbounded read over the line/byte cap returns *guidance* ("use offset/limit"), not a dump.
- **`edit`** — exact match, then a fall-back chain (trailing-whitespace → indentation → block-anchor,
  first-unique-hit wins, reporting which strategy matched); the view carries a unified diff.
- **`write`** — view carries a diff vs prior content (all-additions for a new file).
- **`grep`** — regex by default (`literal` escape hatch for substring).
- **`append`** (lower-risk than `write`), **`read_many`** (survey several files in one node), **`patch`**
  (line-anchored `insert_before/after`/`replace_range`/`delete_range`, resolved against ORIGINAL
  coordinates with overlap-conflict detection).
- **Read-before-write guard** — `edit`/`patch` require the file to have been read (or written) this
  session and refuse if it changed on disk since; the read-set lives on the shared `ToolContext`.

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
flows (you watch nodes execute instead of streaming prose). The `render` module is presentation-agnostic:
`render_pretty` is plain, and `render_styled(ast, &Palette)` lets a surface syntax-highlight the tree (the
CLI passes a tty/`NO_COLOR`/`--color`-aware palette; `Palette::PLAIN` keeps `-o pretty`, logs, and the
non-CLI sinks plain). The CLI renders the highlighted plan + a color-coded risk badge, colored
`→`/`✓`/`✗` markers, a live spinner per running op, and a completion rule with timing. No hidden control
flow: every branch, effect, and approval is visible. Faithful *audit-replay* (reconstruct a past run from
recorded outputs) is
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

## 11. One engine (the free-form loop is gone)

There is exactly **one** engine: `flux-flow::engine::FlowEngine`. Every turn the model is a compiler
front-end — it emits a typed Flux-Lang plan (a graph the runtime executes through `Executor::dispatch`)
or answers in prose. **Pure DAG:** the model has no directly-callable tools at all (only `emit_plan` +
`ask_user`), so even a read is a plan node — a turn is *always* an auditable graph. The old free-form
"one provider-native tool call at a time" loop has been **deleted**, not flag-gated.

**Two modes** (mirroring this tool's plan mode): **normal** = plan + execute each turn; **plan** = plan
only, review/refine, approve to run. A `/plan` toggle in the REPL (with `/run`), and a one-shot `--plan`
flag (show the plan, then on a TTY ask `run it? [y/N]`; piped or `-o json|yaml` just prints it). A bare
prompt runs the engine in normal mode (`-p`/`--agent` are hidden no-op aliases; there is no separate
raw-completion mode). There is **no `FLUX_LANG` flag and no free-form fallback** — a turn the planner
cannot compile fails cleanly (surfaced as the assistant's answer), bounded by the repair loop and the
prose-chat exit. The engine **renders the compiled plan before executing it** (auditable), and the
planner is instructed to express loops/branches as Flux-Lang `repeat`/`when` nodes rather than hide them
inside a `bash` command.

Per turn: compile (pure DAG — only `emit_plan`) → risk-gated execution via
`execute_flow` (per-op approval through the same envelope; destructive ops escalate) → feed each plan's
result back **ephemerally** so the model can iterate (read → fix → re-run) → persist **one** assistant
summary. The persisted session log is pure `user → assistant(text)` alternation: raw op outputs never
re-enter history (the don't-re-send win), which structurally removes the session-shape bug class
(no persisted tool_use/tool_result pairs). The quality bar still holds — a fixed head-to-head dogfood
suite (multi-file edits, read→fix loops, an incident runbook, a Slack/kubectl flow) must show no
regression in success rate, turn count, and p95 latency before this is trusted; `docs/vision.md` reflects
the new claim. (The legacy `flux-agent::Agent` loop remains in-tree as dead code, no surface uses it, to
be removed.)

## 12. Resolved decisions & deferred details

Resolved in design (no longer open):

- **Compiler output (prompt-and-parse).** The provider abstraction has no forced structured output
  (`Request` carries only a `metadata` passthrough; neither the Anthropic nor OpenAI codec sends
  `tool_choice`/`response_format`). So the `compile` front-end **prompts the model to emit the AST as
  JSON, then extracts + parses + validates it, with a bounded analyze→repair loop**. A provider-level
  structured-output seam is future work. Surfaced via `flux --plan [-o json|yaml|pretty] "…"` (plan
  mode), which compiles and shows the AST (the `render` module produces the `pretty` execution-path
  tree). `Node::Lit` holds raw JSON so model-written literals are natural.
- **Pure DAG (the model's only tool is `emit_plan`).** The planner advertises **no directly-callable
  ops** — only the synthetic `emit_plan` (+ `ask_user` when a terminal is attached). *Every* operation,
  **reads included**, is a node in the emitted graph, so a turn is always an auditable plan, never a
  free-form tool call. To gather context the model emits a plan with `read`/`grep`/`glob` nodes; the
  runtime executes it and feeds the result back so it can plan the next step (the engine's multi-round
  loop). The planner is **session-aware** (`view(Session)` lets it reference existing `$symbols`) and is
  told to express control flow as `repeat`/`when` nodes, never shell loops, so the plan stays auditable.
  **Turn completion:** `emit_plan` takes an optional `reply` (a one-line closing message); when the plan
  *completes* the request the model attaches it, and the engine runs the plan, shows `reply`, and ends —
  **one round**, no extra "summarize" call. Omitting `reply` is the read→reason case (loop). Trade-off:
  read-then-act tasks take an extra round (a read is its own plan round); re-enabling read-only research
  is a one-line revert. Verified live — `print hello 3×` plans `repeat max 3 { bash }` and ends in one
  round with `Printed "Hello, World!" 3 times.`; `read README then answer` reads in round 1 and answers
  in round 2 from the fed-back content.
- **The planner is the engine (one engine, no fallback).** `compile::compile_turn` plans a turn from the
  *conversation* and returns `TurnOutput::Plan` (a graph to execute) or `TurnOutput::Chat` (a prose
  answer); `FlowEngine` drives it every turn. **Two modes:** *normal* (plan + execute) is the default;
  *plan* (`/plan` in the REPL, `--plan` one-shot) shows the plan and runs it only on approval/`/run`.
  The free-form loop is deleted — see §11.
  The interpreter (`runtime::execute_flow`) walks the body — `bind`/`call`/`return` plus `when` (typed
  branch) and `repeat` (bounded loop, optional `until`; a `when`/`repeat` *condition* is a node's
  truthiness) — resolving each `$symbol` arg to its stored `Value` (`Value::to_json`, natural form) and
  dispatching through the same `Executor::dispatch` envelope (no new bypass). `await` (cross-turn
  suspend/resume) is the next slice; the engine loop covers iteration meanwhile. `plan_risk` previews
  risk; the default path gates per-op at dispatch (destructive escalates), and `--plan` adds a custom
  `PlanApprover { approved, fallback }` — a non-destructive approved op runs without a prompt, a
  **destructive op still falls through to the fallback** (per-op confirm, or auto under `--yes`). **Arg
  mapping:** the AST keeps positional `Call.args`; at execution they map onto each op's named input by
  its JSON-Schema `required ++ optional` order, and the planner catalog renders the same `op(params)`
  signature so the model emits args in that order. **Symbol interpolation:** `eval_arg` substitutes
  `{{symbol}}` / `{symbol}` tokens inside a string lit with the bound symbol's text (only bound symbols;
  unbound tokens are left verbatim) — so a plan can embed a stored value into a larger string (e.g. a
  `task` prompt). A standalone `$symbol` (a `var` node) passes the whole value as an argument.
- **Auditable display.** The CLI shows plans and tool *inputs* in full (they're model-authored and
  bounded); tool *output* gets a generous preview by default, with `-v`/`--verbose` (`FLUX_VERBOSE`)
  removing all truncation — nothing about what runs is hidden when reviewing.
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
