# Live-session catalog refresh (C-318)

## Decision

Flux publishes complete operation-catalog generations through a side channel,
`LiveToolCatalog`. It does not make `Executor::registry()` interior-mutable and does not add a
`registry_mut` escape hatch.

This follows the ownership shape already used by `DynamicComposites` and `EngineLoopHost`: a
long-lived host owns mutable publication state, while a turn receives an immutable snapshot. The
alternative — putting a lock around the executor registry and consulting it on each lookup — would
allow a refresh between provider planning and dispatch. The model could then see one schema and
execute a different handler or fail because the operation disappeared. It would also make every
catalog read pay synchronization and obscure which generation authorized an in-flight call.

`LiveToolCatalog::try_update` clones the latest complete `ToolRegistry`, applies a fallible update,
and publishes it with one `Arc` swap only on success. `LoadedPlugin::refresh_live` performs that
publication before committing the plugin manifest, preserving C-310's all-or-nothing ordering.

The assembly-time `Executor::registry()` remains stable for assembly validation and compatibility.
Conversational execution uses `Executor::active_registry_snapshot()`: the lexical turn snapshot
when present, or the latest published generation for a direct/one-shot dispatch.

The CLI retains each `LoadedPlugin` for the lifetime of the running agent. `/plugin-refresh <name>`
locks that plugin's existing subprocess, calls `LoadedPlugin::refresh_live`, and publishes into the
same `LiveToolCatalog` the agent samples at its next turn boundary. The standalone
`flux plugin refresh <name>` command remains a one-shot validation/inspection surface; its scratch
registry is not presented as a running-session update.

## Boundary and consistency

The adoption boundary is `FlowEngine::begin_turn_lifecycle`, after the public turn entry point has
acquired the engine's single-active-turn gate and before surfacing or provider request construction.
The engine takes exactly one `Arc<ToolRegistry>` and supplies it to:

- evidence/group surfacing and the advertised operation set;
- `EngineLoopHost` and every `StagedContext` used to build provider schemas;
- Flux-Lang validation, plan-risk calculation, and authored-flow operation catalogs; and
- `RuntimeTurnContext`, from which `Executor::authorize` and `Executor::dispatch` resolve handlers.

A refresh published during the turn changes only the generation available to the next boundary.
An already-resolved handler is itself an `Arc<dyn Tool>`, so withdrawal cannot replace its spec or
drop it while `execute` is in flight. The focused engine test publishes only after the old handler
has entered `execute`, lets that call finish, then proves the next turn can call the gained op and
cannot call the withdrawn one.

Catalog snapshots are executor-affine. Nested SDK runtimes inherit cancellation, session lineage,
identity, and reporter capabilities, but `ExecutionEnvironment::inherit_runtime_turn` removes the
parent catalog before installing that inherited context. The nested executor therefore dispatches
only through the registry its builder deliberately scoped.

Delegation has the complementary rule: `TaskTool` copies both the parent turn's adopted generation
and its stable assembly baseline into `SpawnRequest`. `LocalSpawner` applies only that parent delta
to its explicit child base, then intersects the result with the role and active `with_tools` scope.
The distinction is load-bearing for SDK hosts: their child base may contain child-only operations
the parent never exposes. Those operations survive an unrelated parent refresh, while overlapping
operations receive replacements/withdrawals and scope can only narrow on descent. The child's
`ResourceLimits::independent_copy()` remains unchanged, so every child inherits the configured
numbers and evidence ceiling without sharing the ancestor-held concurrency semaphore.

`[tools] disable` also remains policy across generations. Executors retain both the initially
resolved names (for startup diagnostics) and the original exact/`family.*` expressions. Surfacing
and dispatch re-evaluate those expressions against the adopted generation, so a newly introduced
operation cannot evade an earlier disable merely because it was absent at startup.

## Provider-cache cost

A catalog refresh intentionally invalidates the provider prefix once: operation definitions and
the family index derived from them are part of the cacheable request prefix. The C-318 measurement
uses one running session for three turns. Its serialized `system + tools` catalog prefix was 3,139
bytes before refresh and 3,105 bytes after replacing `echo` with `catalog_gain`. Turn three was
byte-identical to turn two, so the measured invalidation count was exactly one.

That cost is acceptable because it is paid once per accepted catalog generation, at a defined turn
boundary, and subsequent turns regain A-95 byte stability. Coalescing multiple plugin changes into
one complete publication also coalesces their cache miss. Mid-turn churn remains impossible.

## Authority invariants

This wiring does not change C-310's literal capability-containment or retained-operation weakening
checks. `refresh_live` calls the same `prepare_refresh` and `CatalogRefresh::apply` path as
`refresh_into`; it only changes where the complete registry is published. The plugin's load-time
grant remains pinned. In particular, C-322 already added `discovers` to
`LoadedPlugin::pin_granted_authority`, so publishing a refreshed catalog cannot enlist the plugin as
a discovery provider for a product absent from the original grant.

## Verification evidence

- `flux_runtime::tests::live_catalog_refresh_is_adopted_only_at_the_next_turn_boundary`
- `flux_runtime::tests::live_catalog_additions_re_evaluate_exact_and_family_disable_patterns`
- `engine::tests::mid_turn_refresh_preserves_in_flight_dispatch_then_switches_at_the_next_turn`
- `engine::tests::running_session_adopts_refresh_once_and_restabilizes_its_provider_catalog_prefix`
- `catalog_refresh::refresh_live_publishes_for_the_next_turn_without_rewriting_an_adopted_snapshot`
- `catalog_refresh::a_refused_live_publication_keeps_plugin_and_catalog_on_the_old_generation`
- `catalog_refresh::a_withdrawn_op_is_removed_while_an_in_flight_call_completes_under_its_old_spec`
- `flow::tests::streamed_nested_runtime_inherits_reporter_but_not_parent_executor_catalog`
- `tests::spawn_uses_adopted_catalog_generation_without_widening_child_scope`
- `plugin_refresh::repl_refresh_reaches_the_running_agents_live_catalog`
