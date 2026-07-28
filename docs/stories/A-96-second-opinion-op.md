---
id: A-96
title: Second-opinion op — consult a different model for advice, never effects
pillar: Agent
status: done
priority:
epic:
design:
note: "every escalation path today carries authority (sub-agents are policy-bounded but still act); a PURE consult op adds no new authority to the envelope at all — provider/model routing already exists (args.rs:82-97), so this is a read-only op over machinery that ships"
---

# Second-opinion op — consult a different model for advice, never effects

## Goal
Give the agent one cheap move for a hard sub-question: ask a *different* model — typically a
stronger or differently-biased one — and get back **advice**, not actions. The op is pure: it takes
a question plus caller-supplied context, performs exactly one model call, and returns text. It
cannot read, write, spawn, or reach the network beyond that call, so it adds **zero new authority**
to the safety envelope — the cheapest possible fit for flux's thesis, and the surface where
provider neutrality pays off directly (the second opinion can come from a different vendor).

## Acceptance
- [ ] A pure `consult` op (name to be settled) accepts a question + context and a `provider/model`
      spec, performs one model call, and returns the answer as text — failing-first test driving
      `-m mock` and asserting no effect is declared beyond the model call.
- [ ] The op declares `Effect`/`Risk`/`Idempotency` honestly as a **non-mutating** operation and is
      pinned by test as carrying no filesystem, process, or network authority — it cannot be used
      as an egress channel by construction, because the only outbound path is the configured
      provider.
- [ ] The consulted model is resolved through the existing `provider/model` routing
      (`args.rs:82-97`), including the subscription providers, and falls back to the agent's model
      when unspecified.
- [ ] Its cost is attributed to the calling turn: the call emits `call_usage` like every other
      model call, so `flux usage` and the turn's cost line include it. Test asserts the usage event.
- [ ] The returned text enters context as **untrusted content** — it is model output from
      elsewhere and must not be able to close a containment tag (the A-21 lesson). Test covers a
      hostile answer containing the containment delimiter.
- [ ] Surfacing follows the existing rules — the op is not advertised unconditionally if that would
      churn the prompt (see A-95).

## Progress

### 2026-07-28 — implemented

**Orchestrator decisions, recorded as decided:**
- Model-invoked only in this pass — the agent decides when to consult; no `/consult` user command
  yet. Filed as a possible follow-up, not a new story (the op is trivially reachable manually too,
  since a model-invoked op is just a registered tool).
- Per-turn call cap: `[consult] max_calls`, default `flux_cognition::DEFAULT_CONSULT_MAX_CALLS` = 2.
  `0` refuses every call without un-surfacing the op (an operator's hard "off").
- Surfacing is config-gated, not evidence/workspace-gated: the `consult` tool group (in
  `flux_tools::groups::builtin_groups`) is only surfaced by the `consult` ambient signal, which the
  CLI injects into `EngineParts::ambient_signals` exactly once at agent assembly
  (`build_agent_with`) when `cfg.consult.model.is_some()` — mirrors the existing `endpoint` ambient
  signal (D-115) precedent. Never re-probed per turn, so it cannot churn the prompt prefix within a
  session (the A-95 lesson).
- Model resolution chain: op-argument `model` → configured `[consult] model` default → the calling
  agent's own canonical `provider/model` spec. All three tiers route through one injected
  `ConsultFactory` closure built in `flux-cli` from `resolve_cli_provider` (the exact routing
  `-m`/`--model` uses — `flux-cli/src/args.rs:82-97` / `flux-providers::spec`), so subscription
  providers (`claude`, `codex`, `aws`) resolve identically to a top-level `-m` selection. Resolved
  fresh (eager) on every call — never cached — since a consult reply is a cold prompt for whichever
  model answers it (no cache plumbing added, per the story's cost note).

**Design decisions:**
- The op lives in `flux-cognition` (L3) as `ConsultTool`, a sibling to `CognitionPack` but
  registered independently (a fixed pack owns one provider/model; `consult` resolves a different
  one per call via the injected factory — a materially different shape, so a new struct rather than
  a `CognitionOp` variant).
- Purity is declared exactly like the existing model-backed cognition ops: `effects:
  [Effect::Network]`, `access: [AccessKind::Provider]`, `risk: Low`, `idempotency:
  NonIdempotent` — no filesystem/process access at all. `AccessKind::Provider` alone is what
  `flux_runtime::authority_requirements_from_declaration` turns into the `model.invoke` authority
  requirement; no other requirement is generated, which is the structural proof the op adds no
  authority beyond the one provider call (pinned by
  `consult_declares_no_authority_beyond_the_model_call`).
- Usage attribution reuses the **existing** `LoopHost::record_model_usage` path used by
  `flux-cognition`'s own ops (not the sub-agent `subagent.usage` rollup, since consult runs
  synchronously inside the calling turn rather than as a nested session) — the same mechanism
  `flux-flow::engine::record_call_usage_events` already drains into per-turn `CallUsage` events.
  Added a `consult.usage` evidence-log kind (distinct from `cognition.usage`) so a reader can tell
  "this pack's own model" spend apart from "a deliberately different consulted model's."
- Per-turn call cap: added `LoopHost::reserve_consult_call(&self) -> usize` (default no-op `0`) to
  `flux-runtime`, implemented on `EngineLoopHost` as an `AtomicUsize` counter reset in `set_turn`
  alongside the rest of turn accounting (`crates/flux-flow/src/loop_host.rs`). `ConsultTool`
  reserves before spending and refuses (`ToolResult::error`, not a hard dispatch error) once the
  ordinal reaches the configured cap.
- Containment (A-21): the reply is wrapped via `flux_core::{ContextBlock,
  render_knowledge_blocks}` — the SAME neutralization the knowledge-injection path already
  established (its `<knowledge-base>` tag-breakout guard), reused here for a tool result rather
  than a system-prompt block, exactly as directed. Pinned by
  `hostile_answer_cannot_close_the_containment_tag`.
- Config: new `[consult]` table in `flux-config` (`model: Option<String>`, `max_calls:
  Option<usize>`), scalar-override merge (project wins), `deny_unknown_fields`.
- Op name settled as **`consult`** (bare, no dot-namespace) — matches the marquee-verb style of
  `task`/`synth`/`bash` rather than the `ai.*` cognition family, since it is a distinct capability
  (a different model) not a member of the same-model cognition pack.

**Files touched:**
- `crates/flux-cognition/src/consult.rs` (new): `ConsultTool`, `ConsultFactory`,
  `DEFAULT_CONSULT_MAX_CALLS`, `CONSULT_USAGE_OBSERVATION_KIND`.
- `crates/flux-cognition/src/lib.rs`, `Cargo.toml` (added `schemars` dep, `mod consult;` + re-exports).
- `crates/flux-runtime/src/lib.rs`: `LoopHost::reserve_consult_call` default method.
- `crates/flux-flow/src/loop_host.rs`: `EngineLoopHost` counter + `set_turn` reset + trait impl.
- `crates/flux-config/src/lib.rs`: `ConsultConfig` + `Config.consult` + `merge()` wiring + 2 tests.
- `crates/flux-tools/src/groups.rs`: new `consult` tool group gated on the `consult` signal + test.
- `crates/flux-cli/src/execution.rs`: `register_tool_packs` builds the `ConsultFactory` from
  `resolve_cli_provider` and conditionally registers `ConsultTool` when `[consult] model` is
  configured; `build_agent_with` injects the `consult` ambient signal under the same condition.
- `crates/flux-cli/tests/website_contract.rs`: registers `ConsultTool` in the catalog-coverage test.
- `crates/flux-lsp/src/catalog.rs`, `Cargo.toml` (added `flux-core` dep): `consult` registered in
  the LSP authoring catalog alongside `CognitionPack`, so `.flux` authoring gets
  completion/hover/diagnostics for it too.
- Docs: `website/docs/language/ops.md` (new "Second opinion" section + quick-ref row),
  `website/docs/reference/config.md` (new `[consult]` section + representative-config entry),
  `crates/flux-flow/docs/ops-reference.md` (quick-ref row + "Second opinion" section).

**New tests** (`crates/flux-cognition/src/consult.rs::tests`, all passing):
`consult_declares_no_authority_beyond_the_model_call`,
`consult_makes_exactly_one_model_call_and_returns_the_answer`,
`explicit_model_argument_wins_over_configured_default_and_agent_model`,
`configured_default_wins_when_no_explicit_model_is_given`, `agent_model_is_the_final_fallback`,
`empty_question_is_rejected`,
`billable_call_emits_usage_observation_and_publishes_to_the_turn_loop_host`,
`free_call_records_no_usage`, `per_turn_call_cap_refuses_once_reached`,
`hostile_answer_cannot_close_the_containment_tag`. Plus
`flux_tools::groups::tests::consult_group_carries_the_op_and_is_gated_on_the_consult_signal` and
`flux_config::tests::consult_config_parses_and_project_overrides_user_on_merge`.

**Failing-first verification** (temporarily reverted the mechanism in place, ran the test, confirmed
it failed for the intended reason, then restored — done for every behavioral acceptance item):
1. Containment: removed the `render_knowledge_blocks` wrap → `hostile_answer_cannot_close_the_containment_tag`
   failed (`0 == 1` opener/closer count) because the hostile `</knowledge-base>` passed through raw.
2. Per-turn cap: neutered the `ordinal >= max_calls` check → `per_turn_call_cap_refuses_once_reached`
   failed ("the third call must be refused").
3. Usage attribution: disabled the `loop_host.record_model_usage` publish inside the guard →
   `billable_call_emits_usage_observation_and_publishes_to_the_turn_loop_host` failed (`0 != 1`
   published calls). (Note: disabling `guard.finish()` alone did NOT reproduce a failure — the
   `Drop` impl's redundant recording caught it, which is itself the cancellation-safety property
   the guard is designed to provide, not a test weakness.)
4. Purity: added a spurious `Effect::Read` to the spec → `consult_declares_no_authority_beyond_the_model_call`
   failed (`[Network, Read] != [Network]`).

**Gate results (crate-scoped, per the orchestrator's instruction not to run the full workspace
gate — three sessions share `target/`):**
- `codewandler-flux-config`: `cargo test` 32 passed; `clippy --all-targets -D warnings` clean;
  `fmt --check` clean.
- `codewandler-flux-runtime`: `cargo test` 123 passed; clippy clean; fmt clean.
- `codewandler-flux-flow`: `cargo test --lib` 214 passed; clippy clean; fmt clean.
- `codewandler-flux-tools`: `cargo test --lib` 152 passed; clippy clean; fmt clean.
- `codewandler-flux-cognition`: `cargo test` 25 passed (10 new); clippy clean; fmt clean (ran
  `cargo fmt` once to apply two formatter-preferred wraps in the new file, re-verified `--check`
  clean and tests still green after).
- `flux-cli`: `cargo test` (bin + all 7 integration test files, incl. `website_contract`) 174 + 5 +
  2 + 4 + 1 + 3 + 13 = all passed, 0 failed; clippy `--all-targets -D warnings` clean; fmt clean
  (one manual wrap applied by hand rather than a crate-wide `cargo fmt`, to avoid reformatting two
  other sessions' concurrently in-progress files in the same crate).
- `flux-lsp`: `cargo test --lib` 51 passed; clippy clean; fmt clean.
- `flux-codegate` (layering lint): `workspace_respects_layering` passed — no inner→outer edge
  introduced (the two new dependency edges, `flux-cognition → schemars` and `flux-lsp →
  flux-core`, are both legal: an external leaf, and L6→L0).
- NOT run: `cargo test --workspace` / `cargo build --workspace` (per instruction — three sessions
  share `target/`; the orchestrator runs the full gate after merging all three stories).

**Public API note:** `flux_cognition` gains new public items (`ConsultTool`, `ConsultFactory`,
`DEFAULT_CONSULT_MAX_CALLS`, `CONSULT_USAGE_OBSERVATION_KIND`) and `flux_runtime::LoopHost` gains a
new default (non-breaking) trait method (`reserve_consult_call`, default `0`) — both additive, no
existing public signature changed. `flux_config::Config` gains a new field
(`consult: ConsultConfig`) with `#[serde(default)]`, so existing `.flux/config.toml` files keep
parsing unchanged. No breaking change.

**Known gaps / follow-ups (not filed as new stories per instruction to stay in scope):**
- `EngineLoopHost::reserve_consult_call`'s wiring (the atomic counter + `set_turn` reset) is
  exercised indirectly through the `LoopHost` trait contract in `consult.rs`'s tests (via a fake
  host) rather than a dedicated `flux-flow`-level unit test — the mechanism is a 2-line delegate
  structurally identical to the already-analogously-untested `calls`/`turn_calls()` counter, and
  building an isolated `EngineLoopHost::install` test fixture (full `Executor`/`Provider`/
  `FlowStore`) was judged not worth the added surface for this pass.
- No `/consult` REPL/user-invoked command (deliberately out of scope per the orchestrator's
  decision — model-invoked only this pass).

## Notes
- Source: [../research/amp.md](../research/amp.md) — Amp's **Oracle** tool (a separate,
  higher-reasoning model consulted for hard problems), which its manual singles out as
  high-value and recommends invoking explicitly.
- The distinction from sub-agents is the whole point: a sub-agent is a bounded *actor* (it has
  tools, a policy scope, and a workspace); this is a bounded *adviser* with no tools at all.
- Open question worth deciding in the story, not at implementation time: is this model-invoked
  (the agent decides to consult) or user-invoked (`/consult`), or both? Model-invoked is where the
  value is, but it is also where the cost is — so it may want a per-turn call cap.
- Cost interaction: a second opinion is by definition a cold prompt for the consulted model. Expect
  no cache benefit and price it accordingly (see the C-133…C-140 cache work).
