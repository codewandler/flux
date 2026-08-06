---
id: C-595
title: "Stop the authored-segment history budget from discarding a worker that already delivered"
pillar: Core
status: done
areas: [flux-flow, flux-cli, flux-tools]
note: "a story worker committed its full deliverable, then the 512 KiB history ceiling failed the turn 0.4% over — the commit exists, the acknowledgement does not"
---

# Stop the authored-segment history budget from discarding a worker that already delivered

## Goal

A long authored implementation loop must not lose its turn — and its handoff evidence — because
retained history crossed a hardcoded ceiling after the work was already committed. Reading files is
what an implementation loop *does*; a ceiling reached mid-loop should compact or terminate cleanly
with the delivered evidence, not fail the turn.

## Acceptance

- [x] Failing first, `an_authored_segment_over_budget_elides_old_results_instead_of_failing`
      (`crates/flux-flow/src/staged.rs`) proves an over-budget authored history sheds the oldest
      tool-result payloads, comes under the ceiling, and keeps the most recent exchange verbatim.
      `an_authored_segment_within_budget_is_never_rewritten` proves elision is a relief valve, not a
      routine rewrite.
- [x] Crossing the ceiling mid-loop elides rather than discarding the turn; when elision cannot free
      enough, the segment returns through the existing `adaptive_result("chat", …)` seam carrying its
      evidence ledger, which `loop_host.rs` maps to `Ok`.
- [x] A failed worker turn records the session and receipt it proved:
      `a_failed_turn_records_the_session_and_receipt_it_already_proved` and
      `a_turn_that_never_produced_a_receipt_records_no_evidence`
      (`crates/flux-cli/src/board_fleet_cmd.rs`). This also unblocks `fleet rework`, which gates on
      `runtime_session` being a string.
- [x] The ceiling is operator-reachable: `ai_segment` accepts `max_history_bytes`, threaded onto
      `AdaptiveLoopPolicy`, so a Fleet implementation profile can raise it with no new config schema.
- [x] `cargo test --workspace --lib` green; `cargo clippy --workspace --all-targets -- -D warnings`
      clean; `cargo fmt --all --check` clean.

## Progress

- Implemented in three parts: evidence preservation on the failure path (`board_fleet_cmd.rs`),
  elision + clean termination (`staged.rs`), and the `max_history_bytes` knob (`loop_host.rs`,
  `reflect.rs`).
- Ordinary adaptive turns keep the committed refuse-above-512-KiB behavior; only the authored-segment
  path changed.
- Only tool results are elided, never the model's own turns or the goal, so retained history stays a
  valid provider conversation. Error results are left legible — they are the loop's own diagnostics
  and are small.

## Notes

- Observed on Fleet `wave-257`, worker-2, assignment `flux/C-562`, on `claude/opus`:

  ```
  step `ai_segment` failed: authored segment history budget exceeded:
  actual_bytes=526544 limit_bytes=524288
  ```

  That is **2 256 bytes — 0.4% — over** a hardcoded `const ADAPTIVE_HISTORY_LIMIT: usize = 512 * 1024`.
- The work was not lost, which is the point: the worker had already committed `04e31775`
  ("fix(fleet): bound the default fleet status projection") — 661 insertions across 6 files
  including the story file, `CHANGELOG.md`, `WHATS-NEW.md` and
  `crates/flux-cli/tests/board_fleet_cli.rs`. The commit is real and reviewable; only the turn's
  acknowledgement and handoff report were destroyed. Fleet therefore reports the wave as
  "1 of 4 story agent turn(s) failed" for the one story that actually delivered a commit.
- `max_rounds: 64` on the implementation profile makes this reachable on any non-trivial story: 64
  rounds of file reads will routinely exceed 512 KiB of retained history.
- Related: [C-593](C-593-authored-segment-ceiling-escapes-router-family-cap.md) — the same wave's
  predecessor failed all four workers on the authored-ceiling family cap.
