# Design: harden the sub-agent primitive for multi-tenant production

**Status:** proposed (story [D-05](../stories/D-05-sub-agent-hardening.md)) · **Layer:** L3
(`flux-orchestrate`) + L2 seam (`flux-runtime`), surfaced at L6 (`flux-sdk`, `flux-cli`) · **Owner:** Timo

## Why

The sub-agent primitive (`flux-orchestrate`: `LocalSpawner` + `TaskTool` + `Role`/`RoleRegistry`) works,
but it was built and proven in exactly **one** shape: the **CLI** and the **self-improvement loop** —
single process, single tenant, file-defined roles, fire-and-forget. The downstream consumer (managed-agents
story **R-03 → A-05**, the manager agent's `managed-agents-builder` sub-agent) is **multi-tenant** and consumes
flux **by path dependency through `flux-sdk`**. Both roadmaps
label the primitive *"ready to consume"* — that is true at the *primitive* level and false at the *seam,
lifecycle, isolation, and audit* levels.

R-03's acceptance is the bar this design targets:

> - An agent's behaviour can invoke a named sub-agent with a task and receive its result, with the
>   sub-agent's tool calls running through the same `Executor` envelope and account scope.
> - A sub-agent cannot exceed the parent's account/authorization (failing-first test on isolation).
> - Built on flux's sub-agent primitive, not a re-implementation.

Five gaps stand between today's primitive and that bar. This design closes all five, additively — no
redesign of the spawner, the role model, or the one-loop-everywhere engine.

## Current surface (what we build on)

In [`crates/flux-orchestrate/src/lib.rs`](../../crates/flux-orchestrate/src/lib.rs):
- `LocalSpawner` (`:58`) — `new(provider_factory, roles, base_registry, system, default_model, max_tokens)`
  (`:71`) + `.with_authorization(policy, caller, trust)` (`:97`). Fields are fixed at construction.
- `Spawner::spawn(role, task, cancel)` (`:109`) — scopes the toolset to the role allowlist, **removes
  `task`** so children are leaves (`:130`), builds an `Executor` with a hardcoded `SubAgentApprover`
  (`:135`), runs one turn on a `FlowEngine`, returns the final text.
- `SubAgentApprover` (`:36`) — auto-approve non-destructive, deny destructive. Not injectable.
- `TaskTool` (`:332`) — the `task(role, task)` tool; reads `ctx.spawner`; **spawns with a fresh, un-wired
  cancellation token** (`:379–381`); returns `ToolResult::ok(text)` / `error(string)`.
- `Role`/`RoleRegistry` ([`flux-agent/src/role.rs`](../../crates/flux-agent/src/role.rs)) — markdown
  frontmatter (`description`/`model`/`tools`) + body. `RoleRegistry` is constructible from disk
  (`load(dirs)`, `:101`) **and** programmatically (`insert`, `:86`).

The only production wiring lives in [`flux-cli/src/main.rs`](../../crates/flux-cli/src/main.rs): `load_roles`
(`:702`) → `LocalSpawner::new` (`:778`) → `registry.register(TaskTool)` (`:797`) →
`ToolContext::with_spawner` (`:913`). **`flux-sdk` exposes none of this.**

The SDK surface we plug into ([`crates/flux-sdk/src/flow.rs`](../../crates/flux-sdk/src/flow.rs)):
`FlowClientBuilder` (`:85`) → `.build(root)` (`:144`) constructs a workspace-scoped `System`
(`System::new(Workspace::new(root))`, `:149`); `FlowClient` (`:175`) holds `registry: ToolRegistry` +
`system: Arc<System>`, exposes `register_op`/`register_pack` (`:222`/`:229`), and — critically —
`build_executor(&self)` (`:350`) builds a **fresh `Executor` per run** with
`ToolContext::new(self.system.clone())` and **no spawner installed**. That per-run assembly point is where
the spawner must be threaded.

## The five workstreams

### WS1 — SDK consumption seam (closes the "re-implementation" gap; R-03 #3)

**Gap.** A consumer that wants sub-agents must hand-reassemble ~140 lines of CLI wiring (`load roles →
LocalSpawner → register TaskTool → with_spawner`). `flux-sdk` has no seam, so managed-agents would
re-implement the assembly the criterion forbids.

**Design.** Lift the assembly into a reusable config + builder in `flux-orchestrate` (the home crate), and
surface it through the SDK. One construction path; the CLI consumes the same helper (proving reuse, the
D-03 pattern). The config carries an **explicit child tool registry** (`child_base`) rather than reaching
into the parent's assembled registry — this decouples sub-agent wiring from parent registration order (a
consumer registers child-visible ops once, in code) and makes the child's tool surface auditable.

```rust
// flux-orchestrate — the reusable config (single source of truth; replaces the bespoke CLI block)
pub struct SubAgents {
    pub roles: RoleRegistry,                       // in-memory (WS5) or disk-loaded
    pub child_base: ToolRegistry,                  // the tool surface children may be granted (subset per role)
    pub provider_factory: ProviderFactory,
    pub default_model: String,
    pub limits: SpawnLimits,                       // WS2
    pub approver: Option<Arc<dyn Approver>>,       // WS3; None → SubAgentApprover (behaviour-preserving)
    pub auth: Option<(AuthorizationPolicy, Caller, Trust)>,
    pub audit: Option<Arc<EventStore>>,            // WS4; None → ephemeral in-memory (today's behaviour)
}

impl SubAgents {
    /// Build the `Arc<dyn Spawner>` from this config (over `system` for guarded IO). The `task` tool is
    /// registered separately into the parent registry by the surface (CLI/SDK).
    pub fn into_spawner(self, system: Arc<System>) -> Arc<dyn Spawner>;
}
```

```rust
// flux-sdk (L6) — the consumer-facing door, mirroring register_op/register_pack
impl FlowClient {
    /// Attach named sub-agents: register the `task` tool into the client registry and store the spawner.
    /// `FlowClient::build_executor` (flow.rs:350) then installs it via `ToolContext::with_spawner`.
    pub fn with_sub_agents(&mut self, sub_agents: SubAgents) -> &mut Self;
}
```

The single integration point in the SDK is `build_executor`: it gains
`ctx.with_spawner(self.spawner.clone())` when a spawner is present (one line, `None` = today's behaviour).
The parent's own approver (`AllowApprover`/`DenyApprover`, `:352`) is unchanged — the *child's* approver is
`SubAgents.approver` (WS3), a separate gate.

**Acceptance shape.** A hermetic SDK example (`crates/flux-sdk/examples/sub_agent.rs`, `mock` provider, no
API key): build a `FlowClient`, `with_sub_agents` over one in-memory role, run a flow that calls `task`,
read the child's result back — no manual `Executor`/`ToolContext` plumbing. CLI refactors onto
`SubAgents::into_spawner`, unchanged behaviour (existing CLI tests stay green).

### WS2 — Lifecycle limits: cancellation, timeout, configurable caps (safety + cost)

**Gap.** Three holes: (a) the `task` tool **discards the parent's cancellation** and spawns a fresh token
(`:379`), so a hung child can't be stopped; (b) there is **no wall-clock timeout** — only
`max_iterations: 30` (hardcoded, no setter, `:90`) and per-turn `max_tokens`; (c) no aggregate budget
across a delegation. For a tenant-facing service this is a DoS / cost-blowout surface.

**Design.**
- **Cancellation threading (L2 seam).** Add an optional per-turn cancel token to `ToolContext`:
  ```rust
  // flux-runtime
  pub struct ToolContext { /* … */ pub cancel: Option<CancellationToken> }
  impl ToolContext { pub fn with_cancel(self, t: CancellationToken) -> Self; }
  ```
  A cancellable driver (`run_turn_cancellable(.., cancel)` — the CLI/server path) installs its token into
  the turn's `ToolContext` (the engine already derives a per-turn sibling context, `flux-runtime:732`).
  `TaskTool::execute` then threads a **child token** of `ctx.cancel` into `spawner.spawn`, replacing the
  orphan token at `:381`. Cancelling the parent turn now cancels the child. Additive: `ctx.cancel: None`
  (the SDK one-shot `execute` path, which is not driven by a token) preserves today's behaviour — the
  child simply runs to completion as it does now.
- **Configurable limits + deadline-as-cancel.** A `SpawnLimits` value carried by `LocalSpawner`:
  ```rust
  pub struct SpawnLimits { pub max_iterations: usize, pub max_tokens: u32, pub wall_clock: Option<Duration> }
  impl Default for SpawnLimits { /* 30 / inherit / None — today's behaviour verbatim */ }
  impl LocalSpawner { pub fn with_limits(self, limits: SpawnLimits) -> Self; }
  ```
  **The wall-clock deadline fires the child's cancel token, it does not drop the future.** `spawn` spawns
  a timer that cancels the child's token at the deadline; the engine then terminates through the **same
  valid-history cancel path** it already has — *not* a `tokio::time::timeout` future-drop. This matters
  because under WS4 the child writes into the shared tenant store: a hard mid-turn drop could leave a split
  `tool_use`/`tool_result` pair (an AGENTS.md release-blocker invariant), whereas cooperative cancellation
  terminates cleanly. A deadline-triggered stop is surfaced as a typed timeout error in the `task`
  `ToolResult::error` (never a panic).
- **Aggregate budget (stretch).** An optional `Arc<AtomicU64>` token meter shared across a delegation so a
  parent can cap total spend over many `task` calls. Default `None` (unbounded, as today). Ship only if
  WS2's other pieces land cheaply; otherwise note as follow-up in D-05 Progress.

**Acceptance shape.** Failing-first: (1) a child driven by `FLUX_MOCK_HANG` is cancelled when the parent
turn is cancelled (asserts the orphan-token bug is fixed); (2) a `wall_clock` of N ms aborts a slow child
with a typed timeout error and a valid session log.

### WS3 — Pluggable approver + explicit account scope (R-03 #1 & #2)

**Gap A — approver.** The approver is nailed to `SubAgentApprover` (`:135`), which auto-approves anything
non-destructive. But A-05's `managed-agents-builder` calls **control-plane CRUD mutations** that the managed-agents
design says must be **approval-gated by the envelope**. A "create/modify customer agent" call won't trip
`is_destructive()`, so today it sails through. The parent's policy/approver must be able to govern the
child.

**Gap B — account scope.** `auth` is fixed at construction (`:97`); there is no first-class
account/tenant concept, and no isolation test. Isolation rides entirely on the caller building a correctly
scoped `LocalSpawner`.

**Design.**
- **Injectable approver.** `LocalSpawner::with_approver(Arc<dyn Approver>)`; default stays
  `SubAgentApprover` (behaviour-preserving). The `task` line `Arc::new(SubAgentApprover)` becomes
  `self.approver.clone().unwrap_or_else(|| Arc::new(SubAgentApprover))`. managed-agents injects an approver
  that gates mutations; the self-improvement loop keeps the default.
- **Account scope = one spawner per request scope (contract, not new machinery).** Formalize the
  supported multi-tenant pattern: a server builds **one `LocalSpawner` per request/account**, bound to
  that account's `(AuthorizationPolicy, Caller, Trust)` via `with_authorization`, with a workspace-scoped
  `System` and an account-scoped `child_base` tool set. The child inherits exactly that floor — it cannot
  widen policy, cannot reach another account's resources, cannot escalate identity. No `Spawner::spawn`
  signature change is needed; isolation is a property of the per-scope construction, which we **document
  and test**. Two enforcement surfaces, both already in the envelope, both relevant:
  - **Guarded-IO confinement** — for filesystem/process/network effects (the coding-style sub-agents:
    scout/worker). `flux-system` rejects cross-workspace paths and arbitrary egress.
  - **Policy + caller-scope on custom tools** — the surface the **`managed-agents-builder` actually relies on**.
    Its effects are CRUD-API calls, not file paths; isolation is the consumer scoping those tools to the
    authenticated caller's account/scopes, gated by the same `Executor::dispatch` the child runs under.
    flux provides the gate; the consumer must register account-scoped tools (a documented obligation).
- **Failing-first isolation test (both surfaces).** In `flux-orchestrate`: (a) a spawner bound to
  account-A's `System` (confined to A's workspace) whose child task attempts to read account-B's path is
  **denied** at the envelope, and a permissive-but-still-A-scoped policy still cannot escape A's workspace;
  (b) a child granted a custom tool whose `permission_subjects` carry an account tag is **denied** when the
  task targets another account — proving the policy/caller-scope surface the builder depends on. This is
  the concrete artifact R-03 #2 asks for, lifted into flux so the guarantee is owned where the primitive
  lives.

**Acceptance shape.** (1) An injected approver that denies a tagged tool is honoured inside a child run.
(2) The isolation test above fails before per-scope confinement is asserted and passes after.

### WS4 — Child activity in the tenant audit log (transparency; couples with D-02)

**Gap.** Each spawn gets a **throwaway `EventStore::in_memory()` + `FlowStore::in_memory()`**
(`:150–152`). The parent records only a coarse `task(...) → <final text>` evidence marker; the child's
individual guarded effects (reads/writes/bash/CRUD) **never reach the parent/tenant event store**. This
undercuts managed-agents **R-04** (run persistence) and **M4** transparency, and contradicts the docs' "sub-agent
runs recorded in flux-events" claim.

**Design.**
- **Thread the parent store.** `SubAgents.audit: Option<Arc<EventStore>>` (WS1). When set, `spawn` creates
  the child session **in that store** instead of a fresh in-memory one, linked to the parent. To avoid
  churning every `create_session(model)` caller, add a dedicated linked-session entry —
  `EventStore::create_child_session(model, parent: &str)` — rather than changing the existing signature.
  The child's tool calls then land in the same append-only log the parent/tenant reads.
- **Clean termination is a precondition.** Because the child now writes into the **shared** store, every
  child termination path must leave a valid session shape — which is exactly why WS2 implements the
  wall-clock deadline as a cancel (cooperative, valid-history) rather than a future-drop. WS4 depends on
  WS2's termination discipline; sequence WS2 before WS4.
- **Account/agent tag + the session entry ride D-02.** [D-02](../stories/D-02-tenant-event-substrate.md)
  adds the account/agent context tag + account-scoped projections to `flux-events` and will itself touch
  session creation. WS4 ships the **threading seam now** (child events captured, parent-linked) and
  **coordinates the `create_child_session` shape with D-02** so the two converge on one tagged,
  parent-linked entry instead of two. When D-02 lands, tenant-scoped run transparency is a projection over
  one log — *not* a retrofit.
- **Default unchanged.** `audit: None` → ephemeral in-memory child store, byte-for-byte today's behaviour
  (no regression for CLI / self-improvement).

**Acceptance shape.** Failing-first: with `audit: Some(store)`, after a `task` call the store contains the
child's session **and** its inner `tool_call` events, linked to the parent session id; with `audit: None`,
the parent store is untouched (regression guard).

### WS5 — Ergonomics: in-memory roles, structured output, configurable depth

**Gap.** Roles are filesystem-first in practice; `task` returns unstructured text; depth is a blunt
hard-coded `1` (children can never delegate).

**Design.**
- **In-memory roles.** `SubAgents.roles` accepts a `RoleRegistry` built via `insert` (already exists) — so
  a multi-tenant service registers per-account roles in code, no shared `.flux/agents` dir. Add a small
  `RoleRegistry::from_iter(impl IntoIterator<Item = Role>)` for ergonomics. (Mechanism exists; this is a
  thin convenience + the SDK accepting it.)
- **Optional structured output.** A `task` variant (or an optional `result_schema` param) that validates
  the child's final text as JSON against a schema and returns the parsed value — for the builder's "emit a
  valid Flux-Lang definition" case. Default path (plain text) unchanged. Keep minimal; if it grows, defer
  to a follow-up and note it.
- **Configurable depth (replace the blunt guard).** Today children are leaves via **two** mechanisms:
  `registry.remove("task")` (`:130`) *and* the child's `ToolContext` getting no spawner installed (so even
  a stray `task` would no-op with "no sub-agent spawner configured", `:377`). A real depth knob must
  address both: thread a `depth` counter and, while `depth < max_depth`, keep `task` in the child registry
  **and** install a depth-incremented spawner into the child context; strip both at the limit. Default
  `max_depth: 1` keeps both guards exactly as today — children stay leaves, no behaviour change unless a
  caller opts in. Because this touches the recursion-safety guarantee, treat `max_depth > 1` as a
  **deferred stretch** (ship the default-1 depth-aware refactor; gate >1 behind its own test + review).

**Acceptance shape.** (1) An SDK consumer registers a role in memory and spawns it (covered by WS1's
example). (2) `max_depth: 1` still strips `task` from children (regression). Structured output + depth>1
each ship with a focused test or are explicitly deferred in Progress.

## Multi-tenant isolation model (the load-bearing section)

The guarantee R-03 #2 needs is **not** new sandboxing — it is the disciplined composition of four
existing flux mechanisms, made explicit and tested:

1. **Authorization floor.** The child runs under the parent's `AuthorizationPolicy` (`with_authorization`)
   — it can only ever be *narrower*. The policy is evaluated in `Executor::dispatch` before any tool runs.
2. **Guarded-IO confinement.** The child's `System` is the parent's, workspace-confined; cross-account
   paths, symlink escapes, and arbitrary egress are already rejected by `flux-system` (AGENTS.md safety
   invariants). One account = one workspace-scoped `System` = one `LocalSpawner`. *This is the surface for
   filesystem/process/network effects.*
3. **Policy + caller-scope on custom tools.** For a consumer whose effects are **API calls, not file
   paths** — the `managed-agents-builder`'s control-plane CRUD — isolation rides the same `Executor::dispatch`
   gate evaluating the tool's `permission_subjects` against an account-scoped policy + inherited `Caller`.
   flux owns the gate; **the consumer must register account-scoped tools** (subjects that carry the account
   so a cross-account target is denied). This obligation is documented, and WS3's test (b) demonstrates it.
4. **Identity inheritance + approval gate.** `(Caller, Trust)` is inherited (no trust escalation); mutations
   route through the injectable (WS3) approver — the tenant's gate, not a blanket allow.

The contract: **a server constructs one sub-agent scope per request, bound to that account's policy +
caller + workspace + account-scoped tool set.** Isolation is then a structural property, and WS3's
failing-first tests pin both surfaces. This is cheaper and safer than inventing an "account id" parameter
threaded and checked everywhere — it reuses the envelope that already exists and that AGENTS.md forbids
bypassing. The one thing flux *cannot* enforce for the consumer is that its custom tools are correctly
account-scoped; that is called out as an explicit integration obligation, not left implicit.

## Layering & invariants

- `flux-orchestrate` (L3) may depend on `flux-runtime`/`flux-events`/`flux-agent` (L2/L3) — all the new
  types live within layer. `flux-sdk` (L6) consuming `flux-orchestrate` is a downward dep. The
  `flux-codegate` layering lint must stay green.
- The `ToolContext.cancel` field is the only L2 change; it is additive (`Option`) and threaded by the
  engine only. No tool is forced to use it.
- **No bypass.** Every new path still dispatches through `Executor::dispatch`. Sub-agents gain *limits and
  audit*, never a way around the envelope.
- **Valid session shape** on every new termination path (timeout, budget-exhausted, cancel) — covered by
  AGENTS.md's recurring termination-contract invariant.

## Test plan (failing-first, per AGENTS.md)

| WS | Failing-first test | Crate |
|---|---|---|
| WS1 | Hermetic `mock` example builds a `FlowClient` with an in-memory role and drives `task` end-to-end | `flux-sdk` (example + test) |
| WS2 | parent-cancel stops a hung child (`FLUX_MOCK_HANG`); `wall_clock` aborts a slow child with a typed error + valid log | `flux-orchestrate` / CLI hooks |
| WS3 | injected approver denies a tagged tool inside a child; account-A child cannot read account-B's workspace | `flux-orchestrate` |
| WS4 | `audit: Some(store)` captures child `tool_call` events parent-linked; `audit: None` leaves the store untouched | `flux-orchestrate` |
| WS5 | `max_depth: 1` strips `task` from children; in-memory role spawns | `flux-orchestrate` |

Plus the full gate: `cargo build/test/clippy -D warnings/fmt` + `cargo test -p flux-codegate`.

## Sequencing

managed-agents R-03 is **M3 (backlog)** — nothing is blocked *today*. Recommended order by leverage-per-risk:
**WS1 → WS2** (pure upside; unblock consumption + close the DoS/cost surface; enable isolation testing),
then **WS3** (the production isolation bar), then **WS4** (transparency; do the seam now even though D-02
tagging lands separately), then **WS5** (polish; structured-output and depth>1 may defer). Each WS is an
independent, separately-committable slice with its own test.

## Non-goals

- **Not** a new sandbox/VM/container boundary — isolation reuses the existing envelope + guarded IO.
- **Not** the D-02 event tagging itself — WS4 ships only the threading seam; tagging + projections are D-02.
- **Not** distributed/remote sub-agents — `LocalSpawner` stays in-process; a remote `Spawner` impl is a
  separate future story.
- **Not** changing the role markdown format or the one-loop-everywhere engine.
- **Not** removing the leaf-by-default guarantee — `max_depth` default stays `1`.

## Open questions

- **Structured `task` output:** a second tool (`task_json`) vs. an optional `result_schema` param on `task`.
  Lean: optional param, to avoid catalog bloat. Decide at WS5 implementation.
- **Aggregate budget granularity:** per-delegation token meter vs. per-parent-turn. Lean per-delegation
  (`Arc<AtomicU64>` on the spawner). Ship only if cheap.
- **`create_child_session` vs. D-02's tagged entry:** WS4 adds a dedicated linked-session method (not a
  signature change to `create_session`). Confirm its shape with D-02 before landing so the two converge on
  one tagged + parent-linked entry rather than two (coordinate with the D-02 author — likely the same hand).
- **Where the parent's `child_base` comes from in the SDK:** the consumer supplies it explicitly in
  `SubAgents` (decoupled from parent registration order). Confirm this is ergonomic enough, or add a
  `from_client_registry(&FlowClient)` convenience that snapshots the parent's tools minus `task`.
