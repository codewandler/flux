---
id: A-17
title: Revise wiring — the loop routes on $ran.failure; revision rendering with ✓-done prefix
pillar: Agent
status: done
priority:
epic: multipass-agent-loop
design: docs/designs/multipass-agent-loop.md
note: closes the epic loop — agent-loop.flux routes on failure kind/fatality in flux-lang (the loop stays the program), and the surface renders revisions honestly
---

# Revise wiring in the loop + revision rendering

## Goal
Connect the two tracks: the phased `agent-loop.flux` (A-14) consumes the structured failure
contract (A-16) — routing on `$ran.failure.kind` / `$ran.failure.fatal` as plain flux-lang — and
the surface renders the revise flow (`✗ step 4/9 edit failed — revising…`; a revised plan renders
with its reused prefix marked `✓ (done)` and only the new suffix live).

## Acceptance
- [x] `agent-loop.flux` routes on `$ran.failure` (e.g. fatal → distinct feedback phrasing/stop-ask
      path; retryable → revise) — behavior pinned by an engine test:
      `loop_routes_fatal_halt_distinctly_from_retryable`.
- [x] End-to-end revise: mid-plan failure → structured feedback → corrected re-emission →
      prefix fast-forward → completion, in one turn
      (`midplan_failure_revise_and_continue_completes_turn`, mock provider).
- [x] CLI/TUI render the halt line and the ✓-done prefix on revised plans (extends A-15's
      rendering; snapshot tests).
- [x] `flux why`/run-trace shows the true story: executed vs skipped vs failed per statement (the
      L-22/A-16 statement ledger — `StatementCompleted{skipped}`/`PlanHalted`/`PlanResumed` — already
      answers this; exercised end-to-end by the new e2e test via the `echo`-dispatch-count proof).
- [x] Gate green.

## Progress
- (not started — filed 2026-07-02 with the multipass-agent-loop epic.)
- 2026-07-02: Implemented end-to-end.
  - **Loop text** (`crates/flux-flow/assets/agent-loop.flux`): the execute pass's `default` arm now
    binds `$failure = $ran.failure` and routes `do observe "turn.revision", $ran` (failure present)
    vs `do observe "turn.iteration", $ran` (clean round) via `when`/`else` — minimal, keeps the
    25-cap/`until $done` guards and the chat/error `match` intact per the design's "loop stays thin"
    intent; the host already builds all kind-specific guidance/phrasing into `$ran.transcript`.
  - **flux-lang fix (flagged per the story's "keep flux-lang untouched unless genuinely missing"
    clause):** `$ran.failure` (a `jq`-sugar dotted access) needed to read FALSY when the round didn't
    halt. Investigating why the loop's `when $failure` was always true (both `turn.iteration` and the
    fatal-vs-retryable test failed identically) found a genuine gap: `eval_pure_node`'s `Jq` arm
    textified a JSON `null` result via `serde_json::to_string`, producing the literal string `"null"`
    — and `json_truthy`'s string rule reads any non-empty text as truthy (only `""`/`"false"`/`"0"`
    are falsy), so a present-but-null field silently became an always-true condition. Fixed in
    `crates/flux-lang/src/runtime.rs`'s shared `lit_text` helper (`Value::Null => String::new()`,
    matching `ExprVal::from_json`'s existing null→`""` precedent and the `jq` arm now reuses
    `lit_text` instead of a duplicated inline match) — one line, tightly scoped, with a spot-verified
    failing-first regression test (`jq_of_a_present_null_field_reads_falsy`, confirmed failing before
    the fix by temporarily reverting it). All 198 flux-lang tests green after the fix.
  - **Loop host** (`crates/flux-flow/src/loop_host.rs`): `LoopHost::run_plan` is now a thin
    normalizing wrapper over the renamed `run_plan_dispatch` (the unchanged A-16 order of
    operations) — it sets `"failure": null` whenever the dispatch didn't already set it, so the field
    is NEVER a missing key (flux-lang's dotted access errors on that) and the loop's routing never
    breaks a clean turn. `flow.plan`'s observation gained `gather`/`phase`/`resumed` fields (the
    A-15 residual close): `gather` reads directly off the incoming plan's own `settled` signal
    (`""` = still gathering) instead of surface-side inference; `resumed` flags a halt-derived marked
    render. A new `flow.halt` observation (`{step, of, op, kind, fatal}`) fires the instant a plan
    halts, right where the rest of the feedback contract is built.
  - **CLI** (`crates/flux-cli/src/main.rs`): `flow.plan` dispatch prefers the direct `gather` field
    (falls back to the tracked state machine only when absent); `render_plan` renders a `resumed`
    plan's marker-prefixed `plan` text (colored per line via new `style_marked_plan`) instead of
    reconstructing an unmarked tree from `plan_ast`; new `render_halt`/`halt_line` print `✗ step N/M
    <op> failed — revising…` on a `flow.halt` observation. 6 new tests.
  - **TUI** (`crates/flux-tui/src/lib.rs`, `plan.rs`): mirrors the CLI — `UiEvent::Plan` dispatch
    prefers the direct `gather` field; `plan::render` renders a `resumed` plan's marker lines
    (`theme.ok_style`/`err_style`/`muted_style` per line) instead of the full tree; `flow.halt`
    reuses the existing `Notice`/`Sev::Err` machinery (no new `Entry`/`UiEvent` variant needed) via a
    new `halt_line` helper. 3 new tests.
  - **Docs:** `docs/agent-loop.md` (revise-wiring section + `--show-loop` sample), `crates/flux-flow/docs/ops-reference.md`
    (`run_plan`'s `failure`, `observe`'s new kinds, `flow.plan`/`flow.halt`), `LoopHost::run_plan`'s
    doc comment (`flux-runtime`), `RunPlanOp`'s description (`flux-tools`) — all truthed up to the
    new `failure`/`flow.halt`/`turn.revision` shapes.
  - New tests (10 total): `run_plan_failure_field_is_always_present_never_missing`,
    `flow_plan_observation_carries_gather_phase_and_resumed_fields`,
    `flow_halt_observation_fires_with_step_of_op_and_kind`,
    `midplan_failure_revise_and_continue_completes_turn`,
    `loop_routes_fatal_halt_distinctly_from_retryable` (flux-flow);
    `jq_of_a_present_null_field_reads_falsy` (flux-lang);
    `flow_plan_gather_field_is_honored_directly_even_when_state_inference_disagrees`,
    `flow_halt_observation_renders_the_step_and_op`, `flow_halt_dispatches_to_render_halt`,
    `style_marked_plan_colors_each_line_by_its_status_marker`,
    `render_plan_prefers_marked_text_when_resumed` (flux-cli);
    `channel_sink_forwards_flow_halt_as_a_notice`,
    `resumed_plan_entry_renders_marker_colored_lines_not_the_full_tree` (flux-tui).
  - Gate: `cargo build/test/clippy -D warnings` green for `-p flux-lang -p flux-flow -p flux-cli
    -p flux-tui` (198/170/72/31 tests respectively) plus `-p flux-runtime -p flux-tools` (doc-only
    touches there); also ran the FULL workspace `cargo build/test --workspace` (86/86 test-result
    blocks green, 0 failures) and `cargo clippy --workspace --all-targets -- -D warnings` (clean) as
    an extra sanity pass given the flux-lang core-function change; `cargo fmt --all` clean
    (`-- --check` passes).

## Notes
- Depends on A-14 + A-16 (the two epic tracks join here).
- Loop text changes must keep phase-less compatibility (old ejected loops) — verified:
  `old_ejected_loop_text_still_runs` and `phase_less_plan_call_behaves_as_execute` stay green
  unchanged (a phase-less/pre-A-14 loop never binds `$ran.failure`, so this story's change is inert
  for it).
- **Deviation/addition beyond the letter of the story:** touched `flux-lang` (`runtime.rs`'s
  `lit_text`), which the task asked me to avoid unless something was genuinely missing — flagged
  above with the failing-first proof; the alternative (leaving `run_plan`'s "no failure" JSON shape
  as an entirely-absent key, or picking a non-`null` sentinel like `""` to dodge the gap) would have
  either reintroduced the missing-key `jq` runtime error the wrapper exists to prevent, or churned
  several already-shipped A-16 test assertions (`.get("failure").map(|f| f.is_null())`) for no
  benefit — fixing the root cause was the smaller, more consistent diff.
- Residual not pursued (out of scope / no evidence it's needed yet): a dedicated `flux why` CLI
  surface — no such subcommand exists in this codebase; the Acceptance item is satisfied by the
  existing durable statement ledger + the new e2e test's dispatch-count proof, not a new command.
