# Design: Flow-driven session primitive — an authored flow as the conversation driver

**Status:** implemented (2026-07-11) · **Pillar:** Agent · **Layer:** L4 (flux-flow engine) + L2 (flux-runtime `LoopHost` + flux-tools reflexive op) · **Story:** [D-131](../stories/D-131-flow-driven-session-primitive.md) · **Siblings:** [D-132](../stories/D-132-voice-defers-to-flow-suspensions.md) (voice defers to flow suspensions), [D-133](../stories/D-133-annotate-effects-helper.md) (`annotate_effects`)

## Why

Downstream (ai-agent-platform R-20, "deterministic conversation mode") wants to run an **authored
flow as the session's conversation driver**: turn 1 executes the flow to its first top-level
`await` and shows the *flow's own authored prompt* as the assistant turn; each later turn resumes
that suspension deterministically — **no planner in the loop**. The value is a conversation whose
control flow is a reviewable program, not the model: the skeleton is deterministic, and any
non-determinism is a *visibly-bounded* delegation the author opted into.

The machinery is ~90% present (confirmed by a full seam audit, 2026-07-11):

- **Suspend/resume already ships.** `FlowEngine::run_turn_cancellable`
  (`crates/flux-flow/src/engine.rs:284`) checks `take_suspension(session_id)` **first** and, when a
  flow is parked on a top-level `await`, routes to `resume_suspended` (`engine.rs:1086`) — a
  first-class turn with full begin/end-turn + usage accounting (the C-26 fix; no more `turn_id=-1`).
- **The suspension persists** via `FlowStore::save_suspension` / `take_suspension`
  (`crates/flux-flow/src/state.rs:482`,`:505`) — a load-and-delete SQLite row carrying
  `(flow_name, body, node, source)`.
- **The authored prompt already exists in hand.** When `run_top_level`
  (`crates/flux-lang/src/runtime.rs:1015`) hits a top-level `await`, it returns a `FlowOutcome`
  whose `result` field is `last` — *the last non-empty emitted text before the await*
  (`runtime.rs:1089`). That is exactly the "authored prompt."

What is missing is two things:

1. **No fresh-start entry point.** Nothing starts an authored flow *as the driver*. The only
   fresh-run paths are the one-shot SDK `FlowClient::execute` (`crates/flux-sdk/src/flow.rs:380`),
   which deliberately **errors** on a suspension ("the one-shot SDK `execute` path does not support
   cross-turn resume — drive await flows through the engine instead", `flow.rs:637`), and the
   planner-driven agent loop (which is the model *being* the driver — the opposite of this story).
2. **The authored prompt is thrown away.** On suspension the engine surfaces a hardcoded hint,
   `"(awaiting your input — reply to continue the flow)"` (`engine.rs:1157`), instead of
   `outcome.result`.

And one genuinely new capability:

3. **Bounded model-segment delegation** — a flow node that hands a run of turns to the model loop
   under a capability scope + an explicit exit condition, then resumes deterministically. This is
   the "deterministic skeleton with visibly-bounded non-deterministic segments."

This design is therefore **a promotion of existing resume machinery into a first-class session
mode (Phase A), plus one new bounded-delegation primitive (Phase B)** — not a new engine.

## Invariants (verify before ship)

1. **Zero planner invocations on the deterministic path.** A flow driven turn-by-turn through
   `start_flow_turn` + resume must make **no** provider/planner call for the deterministic
   skeleton. Only a Phase-B `ai_segment` may call the model, and only within its declared bounds.
   *Verify:* a mock provider whose call count is asserted `== 0` across a two-`await` driven flow.
2. **One safety envelope, no bypass.** Every op a flow-driven session dispatches — fresh, resumed,
   or inside an `ai_segment` — traverses the same `Arc<Executor>` (authorization → approval →
   guarded IO) the planner path uses. `start_flow_turn` and `ai_segment` add **no** new dispatch
   path. *Verify:* a `RiskApprover` that denies a destructive op blocks it identically whether the
   op is reached via planner, via a resumed flow, or inside an `ai_segment`.
3. **The suspension seam is the only cross-turn state.** `start_flow_turn` persists exactly the
   same `(flow_name, body, node, source)` row `resume_suspended` already consumes, so every
   subsequent `run_turn` routes through the existing suspension-first branch untouched. No second
   parking mechanism.
4. **`ai_segment` cannot exceed its declared capability scope.** An op the segment attempts that is
   outside its `tools` set is denied by the existing `push_cap_scope` narrowing
   (`crates/flux-runtime/src/lib.rs:1076`), exactly as a planner `with_tools` ceiling. *Verify:* a
   segment scoped to `[read]` that attempts `bash` is denied.
5. **`ai_segment` is bounded and always returns control.** The delegated model loop runs under an
   explicit exit condition (round cap, and/or the model completing); on exit, control returns to
   the deterministic flow with the segment's result bound. It can never run unbounded. *Verify:* a
   segment with `max_rounds: 1` against a non-completing mock stops after one round and the flow's
   next node executes.
6. **Prompt fidelity.** The text surfaced on suspension is the flow's last emitted view
   (`outcome.result`) when non-empty; the hardcoded hint remains only as the empty-result fallback,
   so an author who emits nothing before an `await` still gets a usable prompt.

## Approach

### Phase A — `start_flow_turn` + authored-prompt surfacing

**New engine entry point** in `crates/flux-flow/src/engine.rs`, adjacent to `resume_suspended`:

```rust
/// Start an authored flow as the session's conversation driver. Executes the flow fresh to its
/// first top-level `await`, persists the suspension, and surfaces the flow's authored prompt as
/// the assistant turn. Every later turn routes through the existing suspension-first branch.
pub async fn start_flow_turn(
    &self,
    session_id: &str,
    flow: &DraftAst,           // the authored flow (its `body` + optional name)
    sink: &mut dyn AgentSink,
) -> Result<()>
```

It is a near-mirror of `resume_suspended`, differing only in the driver call:

1. `begin_cache_turn` + `begin_turn` (open a first-class turn; **no** user message is recorded —
   turn 1 is flow-authored, not user-authored). Snapshot `subagent.usage` as `subagent_base`.
2. Drive the **fresh** flow via the existing `execute_flow_with_composites`
   (`crates/flux-flow/src/runtime.rs`, wrapping `flux_lang::runtime::execute_flow` →
   `run_top_level(start=0, resume=None)`) over `self.executor` — so invariant 2 holds by
   construction.
3. **On `outcome.suspension`:** `self.flow.save_suspension(session_id, flow.name(), &flow.body,
   susp.node, &susp.source)` (invariant 3), then surface `surface_prompt(&outcome)` as the assistant
   turn; close the turn `"suspended"`.
4. **On completion:** surface `outcome.result` as the final turn; close `"completed"`.
5. Usage via the existing `record_resume_usage` / `finish_turn` helpers.

**`surface_prompt` helper** (used by both `start_flow_turn` and `resume_suspended`):

```rust
fn surface_prompt(outcome: &FlowOutcome) -> &str {
    let p = outcome.result.trim();
    if p.is_empty() { "(awaiting your input — reply to continue the flow)" } else { &outcome.result }
}
```

**One-line fix in `resume_suspended`** (`engine.rs:1157`): replace the unconditional `hint` with
`surface_prompt(&outcome)` on the re-suspension branch, and surface `outcome.result` on completion
(invariant 6, acceptance item 2). No signature or accounting change.

**SDK ergonomics (optional, non-blocking):** the one-shot `FlowClient::execute` error message
(`flow.rs:637`) is left as-is — it correctly points at the engine. No change required for
acceptance; a future `FlowClient` convenience wrapper over `start_flow_turn` can land with D-132.

This satisfies acceptance items **1** (fresh entry point, authored prompt, zero-planner
failing-first test), **2** (authored prompt on re-suspension + completion result), and **4**
(approver applies — a shared-`Executor` test).

### Phase B — `ai_segment`: bounded model-segment delegation

**Vehicle decision (revised after a seam audit): `ai_segment` is a reflexive OP, not a new AST
node.** A new `Node` kind cascades across the whole language surface — round-trip totality
(`crates/flux-lang/tests/roundtrip_property.rs`, L-18: `parse(format(ast)) == ast`), the native
lexer/parser/printer (`parse.rs`/`syntax.rs`/`lower_cst.rs`/`format.rs`), the model-facing
`emit_plan` schema (`schema.rs`), the node-reference docs + `website_in_sync` drift guard, and
highlight/optimize passes. That is an epic-sized surface for one construct. The **reflexive-op**
path avoids all of it: `plan` and `run_plan` are already ops the engine's `EngineLoopHost` provides
via the `LoopHost` trait (`crates/flux-runtime/src/lib.rs:156`), routed through thin `Tool`s in
`crates/flux-tools/src/reflect.rs`. `ai_segment` becomes a **third `LoopHost` method** with the same
routing — authored as an ordinary named-arg call:

```flux
$summary = ai_segment(
  goal: "Collect the caller's name and reason for calling, then summarize.",
  tools: ["read", "datasource.query"],   # capability scope for the delegated leaf ops
  max_rounds: 6,                          # required bound
  until: "slots",                         # optional: exit once $slots is bound & non-empty
)
```

**Why the model loop stays reachable:** the op runs *inside* the authored flow's dispatch, on the
same `EngineLoopHost`, so it re-enters `self.plan()` / `self.run_plan()` directly (Rust method
calls, not op dispatch) — the goal rides in through the existing **`feedback` channel** (an
ephemeral, non-persisted user message, `loop_host.rs:1108`). `EngineLoopHost::ai_segment` runs the
bounded loop:

1. **Scope** (invariant 4): `executor.push_cap_scope(tools)` for the segment's lifetime — the
   dispatch floor (`crates/flux-runtime/src/lib.rs:1226`) denies any leaf op outside `tools`. The
   segment's `plan()` calls are additionally given **`advertised = tools`** so the model never even
   emits an out-of-scope op (`compile_turn` rejects a hidden op at compile). `plan()` dispatches no
   ops itself, so the machinery is never gated by the scope.
2. **Loop** up to `max_rounds`: `plan({feedback, phase:"execute"})` → on `kind:"chat"|"error"`,
   capture the answer and exit (natural completion); else `run_plan()` (leaf ops dispatch under the
   scope) and feed `transcript` back.
3. **Predicate exit** (invariant 5, optional): `until` is a **symbol name**; before each round check
   `self.flow.resolve(session, until)` — exit early once it is bound to a non-empty value. A symbol
   name (not a native condition Node) is the pragmatic v1: it is host-checkable with no
   unbound-variable error (a bare `$var` predicate *errors* on unbound — `runtime.rs:3410`; there is
   no `is_bound` builtin), and it directly expresses the "exit when `$slots` is complete" case.
4. **Return** the answer; the surrounding `Bind` binds it. `push_cap_scope`'s RAII guard pops the
   scope on every exit path.

**Exit condition (v1, user call 2026-07-11):** the first of (a) natural completion, (b) the
required `max_rounds` cap (invariant 5 — always bounded), or (c) the optional `until` symbol
becoming bound & non-empty.

**Engine wiring:** `start_flow_turn` (and `resume_suspended`) must **arm the loop host**
(`self.loop_host.set_turn(...)`) and run the authored flow through the `ChannelSink` + drain loop
(the same plumbing `run_turn_cancellable` uses, `engine.rs:328`) so a segment's `plan`/`run_plan`
events surface live and the borrow of the sink is resolved. For a flow with no `ai_segment` this is
harmless overhead; the authored prompt is still surfaced explicitly after the drain (Phase A
unchanged). `ai_segment` is added to `MACHINERY_OPS` (never surfaced to the model catalog) and
pre-allowed in permissions like `plan`/`run_plan`.

**Touches:** `flux-runtime/src/lib.rs` (`LoopHost::ai_segment`, default = unsupported error so other
implementors don't break), `flux-tools/src/reflect.rs` (the `AiSegmentOp` tool), `flux-flow/src/
loop_host.rs` (`EngineLoopHost::ai_segment`), `flux-flow/src/engine.rs` (arm host + drain in
`start_flow_turn`/`resume_suspended`; register the op). No `flux-lang` change.

> Phase B carries the only real design latitude (vehicle + exit shape, both fixed above). Phase A is
> unambiguous and landed first behind its own failing-first tests; Phase B reuses the same loop-host
> machinery `plan`/`run_plan` already ride.

## Testing

- **A1 zero-planner drive (failing-first):** a two-`await` authored flow, each arm emitting a
  distinct authored prompt; driven via `start_flow_turn` then two `run_turn`s. Assert the three
  surfaced texts are the two prompts + the completion result, and the mock provider's call count is
  `0` (invariant 1).
- **A2 prompt fidelity:** assert the surfaced suspension text equals `outcome.result`, not the
  hint; and the empty-emit case falls back to the hint (invariant 6).
- **A4 approver parity:** a `RiskApprover` denying a destructive op blocks it inside a driven
  session identically to the planner path (invariant 2).
- **B4 cap-scope denial (failing-first):** an `ai_segment` scoped `[read]` that attempts `bash` is
  denied (invariant 4).
- **B5 bounded exit (failing-first):** an `ai_segment` with `max_rounds: 1` against a
  non-completing mock stops after one round; the flow's next deterministic node runs and its result
  binds (invariant 5).

Gate: `cargo test` (flux-flow, flux-lang) · `clippy -D warnings` · `fmt` (root + plugins) ·
`cargo test -p flux-codegate` · plus the invariant tests above.

## Non-goals

- Voice/realtime delegation to flow suspensions — that is **D-132**, which layers on Phase A's
  authored-prompt surfacing.
- Per-node effect badges (`annotate_effects`) — **D-133**.
- A predicate/DSL exit condition for `ai_segment` beyond `max_rounds` + natural completion.
- Any change to the planner-driven turn path, the one-shot SDK contract, or the suspension storage
  schema (Phase A reuses the existing `(flow_name, body, node, source)` row verbatim).
