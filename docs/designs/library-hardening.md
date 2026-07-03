# Design: Library hardening — context, evidence & flux-lang/flow residuals

**Status:** shipped 2026-07-03 (13/13 stories done, failing-first tests + full workspace gate green: 1218 tests, clippy `-D warnings`, fmt, codegate) · **Pillar:** Agent / Core / Language (cross-cutting) · **Layer:** L0–L4 library
core (no CLI/plugin/release surface) · **Owner:** Timo · **Stories:**
context — [A-21](../stories/A-21-knowledge-base-body-escape.md) ·
[A-22](../stories/A-22-served-agents-compaction.md) ·
[A-23](../stories/A-23-cache-breakpoint-cap.md) ·
[A-24](../stories/A-24-context-byte-budget-overshoot.md);
evidence — [C-22](../stories/C-22-redact-durable-evidence-trail.md) ·
[C-23](../stories/C-23-subagent-usage-double-count.md) ·
[C-24](../stories/C-24-observation-flush-failure-watermark.md) ·
[C-25](../stories/C-25-events-db-busy-timeout.md) ·
[C-26](../stories/C-26-resume-turn-telemetry.md);
flux-lang/flow — [L-26](../stories/L-26-optimizer-nested-arg-reads.md) ·
[L-27](../stories/L-27-analyzer-contract-completion-r2.md) ·
[L-28](../stories/L-28-ledger-rehydration-guard.md) ·
[L-29](../stories/L-29-gather-effect-gate.md)

## Why

Three adversarial subsystem audits (context, evidence, flux-lang/flow) surfaced 15 code-confirmed
residual defects **inside already-shipped stories** — silent wrong output, a `<knowledge-base>`
prompt-injection breakout, and a durable secret leak among them. This epic burns them down. It is
deliberately scoped to the **library core**; crate-release and the plugin platform are out.

## Method

For each of the user's three focus areas an independent Opus audit read the actual code (not the
stories), was told what had already shipped so it would not re-file, and was asked for its 5 strongest
**code-confirmed** findings with `file:line` evidence, a concrete failure scenario, and whether a test
already pinned the behaviour. 15 findings survived; every one is a residual gap *within* a shipped
story, not a re-proposal of it. Two same-fix pairs were merged into one story each (L-27 folds the two
analyzer-position gaps; A-24 folds the two byte-budget overshoots), giving 13 stories.

## Findings → stories

Severity: 🔴 silent correctness / security · 🟠 durability / enforcement · 🟡 accounting / hygiene.

### Context management (pillar Agent)

- **🔴 A-21 — `<knowledge-base>` body is never escaped.** `render_one`/`render_one_truncated` emit the
  block body verbatim; only attributes are escaped (`crates/flux-core/src/context.rs:44` vs `:72`,`:97`).
  A retrieved/poisoned datasource record containing a literal `</knowledge-base>` closes the containment
  tag early and lands attacker text as top-level system content — a structural prompt-injection escape on
  the D-07 RAG path. No test covers a body with the close tag. (residual of A-19)
- **🔴 A-22 — served / agentic / SDK agents never compact.** The app agent-target is built with
  `AgentSpec::default()` → `compact_threshold_chars = 0` (`crates/flux-app/src/app.rs:856`,
  `crates/flux-agent/src/lib.rs:158`), which makes `maybe_compact` a no-op
  (`crates/flux-flow/src/engine.rs:758`); only the CLI ever sets it (`crates/flux-cli/src/main.rs:1721`).
  A D-09 agentic channel target bound to one persistent Slack-thread session (`app.rs:709`) re-sends the
  whole growing transcript every turn → linear cost, then a hard context-window error. The flagship served
  path is the unbounded one. (residual of the compaction seam / D-09)
- **🟠 A-23 — the 4-breakpoint prompt-cache ceiling has no guard.** `segmented_system_field` stamps
  `cache_control` on every `cache:true` segment, uncapped (`crates/flux-providers/src/messages/mod.rs:133`);
  on the subscription-claude path prefix + planner-A + phase + base-B = **exactly 4**, Anthropic's hard
  max. The next cache:true system/tool/message breakpoint → HTTP 400 on every planner call. Nothing pins
  ≤4. (residual of A-03 / A-13)
- **🟡 A-24 — context byte budgets overshoot their cap.** `render_knowledge_blocks` appends the omission
  marker *after* the budget check, returning ~57 B over `budget` (`crates/flux-core/src/context.rs:134`);
  `symbols_block_bounded` counts only kept lines' `line.len()` (bytes, not chars) and omits its own header +
  marker from the tally, overshooting `SYMBOLS_CHAR_CAP` (`crates/flux-flow/src/compile.rs:1173`). Neither
  path pins `len <= cap`. (residual of A-19 / A-07)

### Evidence (pillar Core)

- **🔴 C-22 — the durable evidence trail is written unredacted.** The `tool_call` observation (with
  per-token permission subjects) is built *before* the C-13 redactor runs on the tool result
  (`crates/flux-runtime/src/lib.rs:1121` vs `:1176`), and the rendered `plan_text`
  (`crates/flux-flow/src/loop_host.rs:891`) is persisted raw; both flush to `events.db` with no redactor in
  the path. A plan/bash step carrying `Authorization: Bearer sk-ant-…` persists in the clear — readable via
  `/evidence`, `flux usage`, the eval harness, D-02 tenant export. (residual of C-13 / C-14)
- **🟡 C-23 — sub-agent spend is double-counted in the all-sessions rollups.** The parent records a
  synthetic `CallUsage` for the child's total (`crates/flux-flow/src/engine.rs:664`) *and* the child bills
  on its own stream in the shared audit store (`crates/flux-orchestrate/src/lib.rs:349`); `cost_summary_all`
  / `efficiency_all` fold over every stream and sum both (`crates/flux-events/src/store.rs:735`,`:773`).
  Default-on `flux usage` "All sessions" reports each `task` sub-agent's tokens twice. A-08 fixed
  double-*persisting*, not double-*counting*. (residual of A-08 / C-06)
- **🟠 C-24 — the observation watermark advances past failed writes.** `flush_observations` writes
  fire-and-forget then stores the watermark unconditionally (`crates/flux-flow/src/engine.rs:598`). A
  transient SQLite `BUSY`/disk error drops those observations *and* jumps the watermark past them, so they
  are never retried — "crash loses one batch" silently degrades to "a failed write loses that observation
  forever." (residual of C-14)
- **🟠 C-25 — the shared `events.db` sets no `busy_timeout`.** `EventStore::open` enables WAL but no
  `busy_timeout`/`synchronous` (`crates/flux-events/src/store.rs:163`); the in-process mutex serializes one
  process but nothing coordinates a `flux app run --serve` daemon plus a CLI turn on the same
  `~/.flux/events.db`. The second writer gets `SQLITE_BUSY` immediately — `record_message` aborts the turn,
  telemetry is lost. (residual of the event-store unification)
- **🟡 C-26 — await/resume continuations record no turn telemetry.** `resume_suspended` never calls
  `begin_turn`/`end_turn`; it finishes with a hardcoded `turn_id = -1` (`crates/flux-flow/src/engine.rs:719`),
  so observations flush unscoped and no `TurnSummary`/`CallUsage` is emitted. Every A-11 reply-driven
  continuation — including its `task` sub-agent spend — is invisible to `turns()`/`efficiency`/`cost_summary`.
  (residual of A-11 / C-14)

### flux-lang / flow (pillar Language)

- **🔴 L-26 — the optimizer is blind to reads inside object/list call args.** `collect_var_reads`
  hand-rolls recursion and drops `Obj`/`List`/`Fmt`/`Expr` under a `_ => {}`
  (`crates/flux-lang/src/optimize.rs:180`), while liveness and plan-risk route through the exhaustive
  `for_each_node`. `$dir = glob(...)` then `grep({path:$dir})` sees no dependency → both land in one
  `Stage::Parallel` → grep resolves `$dir` before glob binds it; the CSE variant silently aliases a stale
  result. This is the canonical named-arg form (L-09) on the SDK's `execute_plan` production path — silent
  wrong output. (residual of the CSE/dead-step optimizer)
- **🟡 L-27 — analyzer contract completion, round 2.** L-21 closed `each`/`jq`/`parse` but missed the
  `route` selector and `verify` `expect` `eval_arg` positions (`crates/flux-lang/src/runtime.rs:2469`,`:2370`
  vs `analyze.rs:1333`,`:1270`), and `Node::Expr`'s `formula` string is never validated against its own
  `vars` map (`analyze.rs:1272` vs `runtime.rs:3435`) though the scope is explicit and checkable with zero
  false positives. Each costs a wasted repair round on a plan the analyzer should reject with a
  bind-it-first hint. (residual of L-16 / L-21)
- **🟡 L-28 — ledger fast-forward silently continues on a missing rehydration value.** When a skipped
  statement's ledgered value can't be looked up (`if let Some(value) = store.get_value(vid)?`,
  `crates/flux-lang/src/runtime.rs:1143`) the code skips the rebind but still counts the statement as
  fast-forwarded, silently losing the bound symbol; `top_level_bind` also has no `parallel` arm. Latent
  today (in-session store is INSERT-only) — reachable exactly when L-25's cross-store resume lands.
  (residual of L-22, feeds L-25)
- **🟠 L-29 — the gather effect gate under-blocks.** `mutating_ops_in` flags an op only if it is
  `Destructive` or has `Effect::Write` (`crates/flux-flow/src/registry.rs:149`), but the approval path and
  the optimizer treat any non-`Read` effect as mutating (`runtime.rs:464`, `optimize.rs:153`), and the phase
  never restricts the advertised catalog. A `gather:true` "read-only orient" round can emit an advertised
  `Network`/`Process`/`Browser`/`LocalSystem` op (http, `run_plan`, cargo/shell) and it executes — the
  read-only-gather contract is not enforced. (residual of A-13)

## Sequencing

Do the 🔴 tier first — all are small-to-medium and currently unpinned by any test: **L-26 → A-21 → C-22 →
A-22 → C-23**. Then the 🟠 enforcement/durability tier (**L-29, C-25, C-24**) and the 🟡 hygiene tier
(**L-27, A-23, C-26, L-28, A-24**). Stories are independent — no hard ordering beyond "correctness/security
before hygiene." Each ships with the failing-first test named in its Acceptance.

## Out of scope

Crate publishing (the `codewandler-flux-*` vanity-prefix work) and the plugin platform hardening epic
(D-46..D-49) — explicitly deprioritized. This epic touches no CI, no release plumbing, and no `plugins/`
workspace.

## Verified-clean (audited, not defects)

Recorded so a later pass doesn't re-audit: L-08 `build_ctx` eviction (stable tier sort, drop-and-continue);
symbols block newest-first (`view()` `updated_at DESC`); `trim_tool_output`/`truncate_str` UTF-8 safety;
compaction persists prose `Text` not `ToolUse`/`ToolResult` (no tool-call loss); `stmt_hash16` determinism
(BTreeMap, `preserve_order` off); the C-17 emit_plan gate ordering; nested-`await`/`checkpoint` rejection;
`ResumeLedger::fold` latch logic.
