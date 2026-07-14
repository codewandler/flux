# Architecture review — 2026-07-14

**Status:** closed 2026-07-14; all 19 findings implemented and verified
**Pillars:** Agent / Core / Language
**Layers:** L0–L6 plus the nested plugin workspace
**Audit snapshot:** `c324b5a8095e` plus the then-dirty worktree; findings rechecked while filing on
`6e691962a1e4` (`v0.24.0`)
**Epic:** `architecture-review-2026-07-14`

## Outcome

All 19 findings are implemented, acceptance-checked, and verified. The four release blockers are
closed: A2A push uses scoped DNS-aware egress, executor assembly always carries policy and identity,
project metadata stays behind a confined guarded-IO boundary, and planning/dispatch share typed
resource-aware authority requirements. The lifecycle fixes moved turn, child-task, and App-delivery
ownership to their shared runtime owners; the consolidation work then removed the parallel assembly,
registry, plugin, web-search, and parser paths that had allowed those contracts to drift.

This document preserves the original review evidence below as a historical snapshot and records the
implemented outcome for every resulting story. No finding remains `ready` or in progress.

## Implementation closeout

### Release blockers

- **C-59:** registration and delivery re-run scoped DNS-aware URL authorization; the pooled push
  client refuses redirects, so notification credentials cannot cross origins. Internal address
  families, DNS rebinding, private grants, realm isolation, timeouts, and no-retry behavior are
  covered at the delivery boundary.
- **C-60:** `ExecutionAuthorization` and `ExecutionEnvironment` make policy, caller, and trust a
  construction invariant. SDK, App, and `AgentSpec` auto-approval tests prove approval cannot widen
  the authorization floor.
- **C-61:** project context, config, roles, and skills load through project-confined `System`
  instances; config persistence uses guarded atomic replacement. Symlink/retarget fixtures and the
  raw-project-metadata codegate keep this boundary closed.
- **C-62:** `AuthorityRequirement` is the shared plan/dispatch contract for concrete resources and
  semantic actions. Catalog, plugin-capability, policy-denial, and sink-backed `web.fetch`/
  `web.crawl` tests pin exact requirements.

### Lifecycle and correctness

- **A-85:** strict, path-aware role loading fails before provider construction or dispatch;
  omitted `tools` and explicit `tools: []` retain their intentionally different meanings.
- **A-86:** adaptive, authored, and resumed work use one turn lifecycle. Cache-generation,
  cancellation, recoverable-checkpoint, history, usage, restart, and voice tests cover the common
  path.
- **A-87:** `FlowEngine` now owns the single-active-turn gate. Same-engine isolation, independent
  engine overlap, nested no-deadlock, and lexical turn-context tests prove the invariant.
- **C-63:** the shared Bedrock factory keeps the lazy expiry-aware resolver, deterministic region,
  and static-credential compatibility without runtime blocking or process-environment mutation.
- **A-88:** one supervised child-task owner handles success, error, timeout, and cancellation with
  cooperative shutdown plus a bounded reap backstop. Hanging provider/tool and nested cancellation
  tests prove terminal and usage records occur once.
- **A-89:** the ownership finding was confirmed. One lazy App-owned actor is the sole trigger router
  for `run`, direct delivery, and public bus roots; broadcast subscriptions are observation-only and
  the adapter-local gate is gone. Per-App causal tags plus direct, wrapped, run-overlap,
  interleaving, and cross-App tests prove cascade isolation, ordering, and exactly-once effects.
- **C-64:** tool and plugin registration is source-aware, atomic, fallible, and duplicate-rejecting;
  intentional replacement is explicit. Runtime, SDK, plugin-host, and first-party collision tests
  pin catalog/handler identity.
- **C-65:** dependency layering uses Cargo package metadata and architecture scanners understand Rust
  syntax, aliases, both command APIs, both workspaces, and project-metadata IO. Fixtures cover each
  former false-negative shape.
- **C-66:** cognition usage is retained independently of success, error, or drop. Provider-error,
  cancellation, zero/success, SDK, event/cost, and sub-agent tests prove exact-once accounting.

### Simplification and consolidation

- **C-67:** one `ExecutionEnvironment` owns shared executor mechanics while surfaces retain policy
  choices. A production conformance test exercises CLI, App, SDK `Client`, and SDK `FlowClient`
  against the same registry, authority, identity, redactor, and guarded root.
- **C-68:** host-kit now provides typed input/output registration and a path-aware shared decode
  contract. The intended phased representative cutover covers websearch's simple `provider.list`
  and flex-heavy `search`, plus Jira attachment list/get; every remaining first-party handler is
  explicitly `operation_flexible`, with the complete matrix documented in
  `plugins/TYPED-MIGRATION.md`.
- **C-69:** the guest feature excludes host transport/runtime, hooks, signing, and archive stacks;
  host-kit selects it explicitly. The structural tree guard records the normal dependency reduction
  from roughly 237 packages to 80 and rejects host-stack regressions. A clean representative
  release build fell from 41.106 s / 2,014,936 bytes to 15.098 s / 1,608,624 bytes, eliminating the
  host-only compile burden without growing the guest binary.
- **C-70:** the first-party websearch plugin is the sole Tavily/DuckDuckGo owner and projects the
  compatibility name `web.search`; keys remain host-resolved and egress remains host-guarded. The
  duplicate native client and `flux-tools` HTTP dependency are gone.
- **C-71:** CLI, server A2A, SDK execution, TUI, plugin host/protocol, and GitLab/Slack/Jira internals
  are responsibility-focused modules inside the existing crate/binary map. Transition,
  execution-kernel, command, manifest, and contract parity tests protect the moves.
- **L-80:** the tolerant CST is the sole accepting Flux-Lang parser and structured CST decoding feeds
  strict `parse`/`parse_program`; the legacy accepting implementation is removed. Corpus, all-node
  round-trip, range/LSP/module/workbench, tolerant-input, and sync tests cover the cutover.

## Scope and method

- Reviewed all 37 root-workspace packages and all 21 nested-plugin workspace packages.
- Inspected dependency direction, high-churn/large modules, provider construction, runtime dispatch,
  agent/flow lifecycle, SDK/App/server assembly, plugin manifests/host callbacks, and architecture
  gates.
- Grounded every safety finding against flux's invariants: authorization → approval → guarded IO,
  workspace confinement, deny-by-default plugin capabilities, guarded URL resolution, and
  provider-valid session history.
- Ran the full root and plugin build/test/clippy/fmt gates before filing these stories; see
  [Verification](#verification).

## Original ranked findings → completed stories

The descriptions in this section capture the pre-implementation state found by the audit; the
closeout above is the current architecture.

### Release blockers

1. **A2A push delivery bypasses guarded egress** →
   [C-59](../../stories/C-59-guard-a2a-push-scoped-egress.md). `push_url_allowed` rejects only
   literal private addresses and accepts arbitrary hostnames; delivery then uses a normal
   redirect-following `reqwest` client. A DNS name resolving to loopback/private/link-local/CGNAT,
   or a public URL redirecting there, can receive an authenticated blind POST. The fix must reuse
   `flux_system::net` resolution and scoped private-network grants.

2. **Public executor assembly can omit authorization entirely** →
   [C-60](../../stories/C-60-require-policy-identity-executor-assembly.md). `Executor::new` defaults
   to `policy: None` and a synthetic privileged identity; `AgentSpec`, SDK, and App construction use
   it. Auto-approval on those paths therefore has no policy floor despite the documented mandatory
   envelope.

3. **Automatic project metadata follows workspace-escaping symlinks** →
   [C-61](../../stories/C-61-confine-project-metadata-io.md). Project context uses raw filesystem
   reads, so a repository-controlled `AGENTS.md`/`CLAUDE.md` symlink can inject any host-readable
   file into model context. Project skill discovery and config persistence have related raw-IO
   seams, including a write-through-config-symlink case.

4. **Effects do not form one enforceable authority contract** →
   [C-62](../../stories/C-62-typed-authority-requirements.md). Generic `Effect::Read` is translated
   to `workspace.read` even for pure, datasource, endpoint, and integration-plugin operations.
   Plugin read presets omit their real network/connection effect. Semantic tags such as
   `write_db`, `money`, and `delete` reach analysis but are not evaluated by `Executor`; unknown tags
   are silently dropped. C-58 and D-138 added honest disclosure/catalog metadata, not dispatch-time
   enforcement.

### Lifecycle and correctness

5. **Malformed role metadata widens to inherited tools** →
   [A-85](../../stories/A-85-fail-closed-role-metadata.md). YAML failures become default metadata,
   where `tools: None` means the entire parent catalog; unreadable role files are silently skipped.

6. **Suspended-flow resume is a divergent turn implementation** →
   [A-86](../../stories/A-86-unify-fresh-resumed-turn-lifecycle.md). Resume bypasses both the
   supplied cancellation token and the op-cache turn boundary. A pre-`await` cached read can be
   replayed after an external edit, and a resumed hanging operation cannot be cancelled through the
   public API.

7. **The single-active-turn invariant is enforced by callers, not `FlowEngine`** →
   [A-87](../../stories/A-87-flowengine-single-active-turn.md). The engine exposes concurrent
   `&self` turn methods while `EngineLoopHost` stores mutable turn-global sink, identity, usage,
   receipts, and audit state. SDK/server gates are partial conventions and the raw engine remains
   public.

8. **The shared Bedrock factory defeats lazy credential refresh** →
   [C-63](../../stories/C-63-preserve-bedrock-lazy-chain.md). It uses `block_in_place`, materializes
   temporary credentials into process environment, and constructs an environment-backed provider,
   bypassing C-37's existing lazy expiry-aware chain resolver.

9. **Parent cancellation can strand an audited sub-agent turn** →
   [A-88](../../stories/A-88-supervise-child-cancellation.md). The timeout branch cancels and awaits
   cleanup; the parent-cancellation branch drops the child future without the same durable terminal
   path.

10. **App delivery serialization ownership was confirmed and moved to `App`** →
    [A-89](../../stories/A-89-app-delivery-serialization.md). Direct calls and two independent
    wrappers reproduced the ownership gap, and final audit exposed `App::run` as a second consumer.
    One App-owned actor now covers `run`, direct delivery, and bus roots; observer subscriptions
    cannot route triggers, and per-App causal tags pin cascade isolation without cross-App leakage.

11. **Registries and architecture gates fail open** →
    [C-64](../../stories/C-64-reject-duplicate-operation-registration.md) and
    [C-65](../../stories/C-65-harden-architecture-gates.md). Tool and plugin-operation registration
    silently overwrite handlers/specs. The layer gate misses renamed and target/build dependencies,
    while the raw-process scanner misses aliases and deliberately ignores Tokio process creation.

12. **Cognition drops billable usage when a stream later fails** →
    [C-66](../../stories/C-66-retain-cognition-error-usage.md). Usage accumulated before a declared
    provider error is discarded because recording occurs only after successful model completion.

## Simplification and consolidation

1. **One execution-environment builder** →
   [C-67](../../stories/C-67-centralize-execution-environment-assembly.md). CLI agent/App paths,
   `AgentSpec`, SDK, and App independently compose workspace, registry, plugins, policy, identity,
   approver, events, and context. This duplication produced the optional-policy and split-root
   defects. Consolidation must follow C-60/C-62 so it reuses the fixed contract rather than
   inventing another one.

2. **Typed plugin execution, not schema-only structs** →
   [C-68](../../stories/C-68-typed-plugin-handlers-output-schemas.md). Roughly 300 input structs
   derive model-facing schemas but handlers manually parse `serde_json::Value`; the type is never
   deserialized, so schema and behavior can still drift. Bind handler input to the derived type,
   retain an explicit compatibility escape hatch, and adopt D-164 output schemas for stable ops.

3. **Guest/protocol feature partition** →
   [C-69](../../stories/C-69-partition-plugin-guest-dependencies.md). `host-kit` currently inherits
   host-only HTTP, credentials, QuickJS, signing, and archive dependencies through `flux-plugin`.
   Prefer Cargo feature partitioning before undoing the prior crate consolidation.

4. **One guarded web-search implementation** →
   [C-70](../../stories/C-70-consolidate-web-search.md). Native `web.search` exposes an API-key
   parameter, reads environment secrets, and owns a second HTTP client, while the first-party
   websearch plugin already provides Tavily plus DuckDuckGo through host capabilities.

5. **Internal modules instead of architectural crate churn** →
   [C-71](../../stories/C-71-decompose-high-churn-modules.md). Split CLI assembly/commands, SDK
   execution branches, server A2A lifecycle, TUI state/render/controller logic, plugin-host
   protocol/install/hooks, and the largest integration plugins along responsibility lines. Do not
   merge `flux-agent`, `flux-flow`, and `flux-orchestrate`; their definition/engine/orchestration
   boundary is healthy.

6. **One accepting Flux-Lang parser** →
   [L-80](../../stories/L-80-complete-cst-parser-cutover.md). CST lowering still delegates semantic
   acceptance to the legacy parser. Complete the L-59 cutover in a compatibility-focused story and
   keep the current AST, diagnostics, and formatter behavior pinned.

## Completed sequence

The 19 stories followed this dependency-aware order; all stages are now complete:

1. **Release blockers:** C-59 → C-60 → C-61 → C-62.
2. **Turn lifecycle:** A-85, A-86, A-87, C-63, then A-88/A-89.
3. **Fail-closed infrastructure:** C-64, C-65, C-66.
4. **Consolidation on the repaired seams:** C-67, C-68, C-69, C-70.
5. **Behavior-neutral structure and language debt:** C-71 and L-80.

C-67 followed C-60/C-62. C-68 established the final guest API before C-69 measured its feature
boundary, and C-71 followed the lifecycle/assembly work so code was moved only once.

## Boundaries to preserve

- Keep the strict L0–L6 dependency direction and the `flux-codegate` classification map.
- Keep process creation centralized in `flux-system`; strengthen the guard rather than add
  exceptions.
- Keep integration-plugin privileged IO behind host capabilities. `pack-index` remains a release
  utility, not a guest plugin runtime.
- Keep `flux-lang` effects injected through host/store/sink traits and keep `flux-flow` as the L3
  engine/facade.
- Keep `flux-agent`, `flux-flow`, and `flux-orchestrate` separate.
- Prefer in-crate modules and Cargo features over new crates unless a measured API/dependency reason
  requires a split.

## Verification

The final closeout passed the root dev-loop gates:

- `cargo build --workspace`
- `cargo test --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo fmt --all -- --check`
- `cargo test -p flux-codegate`
- `git diff --check`

The nested plugin workspace passed workspace check/clippy plus the affected host-kit and integration
plugin suites. Focused verification also covered A2A conformance, FlowEngine lifecycle/concurrency,
sub-agent cleanup, App/channel delivery concurrency, Bedrock factory lifecycle, registry collisions,
typed authority, guarded metadata, SDK execution parity, plugin guest dependency boundaries,
websearch/Jira typed contracts, and Flux-Lang CST/LSP/sync suites. The acceptance sections of the 19
completed stories name the durable regression contract for each change.
