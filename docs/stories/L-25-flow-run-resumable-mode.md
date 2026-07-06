---
id: L-25
title: Pre-authored flow-run resumable mode — reified halts for `flux flow run`
pillar: Language
status: done
epic: multipass-agent-loop
design: docs/designs/multipass-agent-loop.md
note: extend the resumable entry point to the engine's non-loop flow path (authored .flux flows), and revisit whether the ledger subsumes checkpoint for that path too
---

# Pre-authored flow-run resumable mode

## Goal
Give authored flows (`flux flow run`, journeys) the same reified-halt + ledger + fast-forward
machinery the loop gets, so a failed long flow can be corrected and continued instead of re-run
from the top (today: checkpoint fast-forward only, defeated by any edit).

## Acceptance
- [x] `flux flow run` (and the engine flow entry) can opt into resumable mode; a halted authored
      flow reports the structured halt; a corrected re-run fast-forwards the matching prefix.
- [x] Checkpoint interplay decided and documented (ledger subsumption vs coexistence for authored
      flows); `once`/`saga` invariants re-verified on this path.
- [x] Gate green.

## Progress
- 2026-07-06: Implemented per the settled surface design (see the design doc's "L-25: pre-authored
  resumable mode" section, 2026-07-06). `flux flow run <file> --resumable` opts a fresh run into
  reified halts (a failure, or the L-24 `Awaiting` pause): on a halt it prints the A-16-style
  structured report (✓/✗/· marked statement tree + a machine-readable `failure` JSON + the session
  id) and exits non-zero instead of erroring the whole run. `flux flow run <file> --resume
  <session|last>` re-parses the (corrected) file, folds that session's halt ledger via the SAME
  `ResumeLedger`/`FlowStore::open_halted_plan` machinery `run_plan` uses (no new table), and
  fast-forwards the matching completed prefix (values rehydrated) before executing from the first
  changed statement; `last` resolves the most recent halted session whose halted plan's key is
  prefixed by the flow's declared name (an unnamed flow needs the explicit session id — nothing
  else safely disambiguates it from an ordinary chat turn's inner `run_plan` halt in the same
  store). Journeys/`resume_suspended` (A-11) are untouched — out of scope per the settled design.
  - Engine seam (`crates/flux-flow/src/runtime.rs`): two new public helpers reused by the CLI —
    `render_halt_report` (the human-facing halt rendering, reusing `render_statement` and the loop
    host's own `failure_kind_label`) and `denied_reemission_guard` (the design Part-2 denial guard,
    factored out so the authored path enforces "denied statements never re-dispatch unchanged"
    (A-16) without going through `run_plan`). `execute_flow_resumable_with_composites` (already
    public, previously loop-host-only by convention) is now the shared entry point for both.
  - CLI seam (`crates/flux-cli/src/main.rs`): `FlowAction::Run` gained `--resumable`/`--resume`;
    `run_draft_ast_with_composites_resumable` is the new resumable variant (`run_draft_ast`/
    `run_draft_ast_with_composites`, used by `flux preset --run`, are untouched wrappers passing
    `false, None` — zero behavior change for every other caller); `build_agent_with` gained a
    `session_override` parameter so `--resume` targets the exact halted session instead of minting
    a fresh one; `resolve_resume_session` resolves `last` by name-prefix search over
    `EventStore::list`.
  - Checkpoint interplay: documented as a one-paragraph rationale directly under `checkpoint` in
    `crates/flux-lang/docs/reference.md` — coexistence (composed as `start =
    ledger_end.max(checkpoint_next)`, already how `run_top_level_resumable` computes it), not
    subsumption, because their invalidation semantics differ on purpose (any edit defeats
    `checkpoint`; the ledger tolerates a suffix edit).
  - Tests (failing-first, `crates/flux-flow/src/runtime.rs`): `render_halt_report_marks_prefix_and_embeds_machine_readable_failure`,
    `denied_reemission_guard_blocks_only_the_unchanged_denied_statement`,
    `resumable_authored_path_denied_statement_would_redispatch_without_the_guard` (proves the guard
    is load-bearing, not redundant with the interpreter), `resumable_authored_path_never_refires_once_across_fast_forward`,
    `resumable_authored_path_saga_recompensates_consistently_on_resume` (through the REAL
    `Executor`/`FlowStore` engine adapter, not just flux-lang's generic fixtures). CLI-level:
    `resolve_resume_session_passes_through_literals_and_last_matches_by_flow_name` in
    `crates/flux-cli/src/main.rs` (the harness has no precedent for a full mock-provider
    end-to-end CLI test — no existing test builds a `FlowEngine` at all — so the CLI-owned
    session-resolution logic is unit-tested directly; the full flag-to-execution wiring was instead
    proven with a live manual smoke: halt → structured report → edit → `--resume <session>` →
    fast-forward (no re-dispatch of the completed prefix) → success; `--resume last` name-matching
    across two halted sessions; and a denied `write` → `--resume` on the unchanged file → guard
    blocks, file never created, exit 1).
  - Gate: `cargo build/test/clippy --all-targets -- -D warnings` (workspace), `cargo test -p
    flux-codegate`, `cargo fmt --all -- --check` all green; `cargo test -p flux-flow -p flux-lang`
    full run green (241+240 tests), confirming L-23/L-24's fresh tests are undisturbed.

## Notes
- Depends on L-22. Surface question (how a human corrects an authored flow mid-halt) resolved by
  the design's 2026-07-06 "L-25: pre-authored resumable mode" section: `--resumable` + `--resume
  <session|last>`, no new `flux flow resume` subcommand.
