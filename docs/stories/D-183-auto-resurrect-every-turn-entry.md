---
id: D-183
title: Run the auto-resurrect step on every turn entry — SDK stream/start_flow and the CLI REPL/TUI
pillar: Agent
status: done
epic: deterministic-agent-lab
design: docs/designs/deterministic-agent-lab.md
priority: 2
note: "review finding (2026-07-28): only send/send_with (SDK) and one-shot flux run (CLI) resurrect; stream/start_flow/REPL/TUI turns run on top of a crashed turn"
---

# Run the auto-resurrect step on every turn entry — SDK stream/start_flow and the CLI REPL/TUI

## Goal
`auto_resurrect` fires only in `Session::send` and `Session::send_with`
(`crates/flux-sdk/src/session.rs`); `Session::stream` and `Session::start_flow` skip it, even
though `ClientBuilder::auto_resurrect` documents "a turn entry first resurrects". On the CLI,
`resurrect_on_open` is wired only into the one-shot `run_agentic` path
(`crates/flux-cli/src/execution.rs`) — the REPL and the TUI never resurrect.

Confirmed failure: a durable session crashes mid-turn; the embedder resumes via `stream()` (or the
user via the REPL/TUI). The new turn runs on top of the still-open crashed turn, and
`flux_flow::resurrect::interrupted` (which finds the *last* turn with no `ended_at_ms`) still
returns the stale turn — so a **later** `send()` resurrects it after newer turns already ran,
appending a stale assistant message out of conversational order.

## Acceptance
- [x] `Session::stream` and `Session::start_flow` run the same `auto_resurrect_step` as
      `send`/`send_with` (same turn-guard, same `TurnOutput::resurrected` reporting or the stream
      equivalent).
- [x] Failing-first test: crash a durable session, resume via `stream()` → the interrupted turn is
      finished first and a later `send()` finds nothing to resurrect (no out-of-order resurrect).
- [x] The CLI REPL and the TUI run the resurrect-on-open step when entering a session with an
      interrupted turn (same loud reporting, same `FLUX_AUTO_RESURRECT=0` opt-out).
- [x] Docs updated back: the scoping caveats added on 2026-07-28 to `website/docs/agent/cli.md`,
      `website/docs/sdk/agent-lab.md`, `docs/designs/deterministic-agent-lab.md`, and the
      `flux sessions` hint ("one-shot `flux run` turn") are removed once every entry point
      resurrects.
- [x] `resurrect::interrupted` additionally refuses (or warns loudly) when the interrupted turn is
      not the session's most recent turn — the out-of-order tail-guard even if an entry point is
      missed again.

## Progress
- 2026-07-28: Implemented end-to-end.
  - `crates/flux-flow/src/resurrect.rs`: added the shared `resurrect_on_open` step (env-var
    opt-out `FLUX_AUTO_RESURRECT`, `OnOpenLine` reporting enum) used by every entry point, plus
    the out-of-order tail-guard in `interrupted()` — refuses loudly (`crash_err`) when the open
    turn is not `events.turns(session)`'s last turn. New tests:
    `interrupted_refuses_when_the_open_turn_is_not_the_sessions_most_recent_turn`.
  - `crates/flux-sdk/src/session.rs`: `start_flow` now runs `auto_resurrect_step` before starting
    the flow. `stream()` clones the `Session` handle into its spawned task and runs
    `auto_resurrect_step` on the same `ChannelSink`/`tx` the new turn uses, before the new turn —
    so the resurrected turn's own events stream out first, and `TurnOutput::resurrected` (from
    `finish()`) carries the report, exactly like `send`/`send_with`.
  - `crates/flux-sdk/tests/resurrect.rs`: added
    `stream_resurrects_an_interrupted_turn_before_its_own_new_turn_runs` (the failing-first
    acceptance scenario) and `start_flow_resurrects_an_interrupted_turn_first`. Updated the two
    existing `auto_resurrect(false)`/in-memory tests: once a new turn runs on top of a still-open
    crashed one, `interrupted()` now refuses loudly (the tail-guard) instead of reporting the
    stale turn forever — the exact hazard this story closes.
  - `crates/flux-cli/src/execution.rs`: `resurrect_on_open` is now a thin colorizing wrapper over
    `flux_flow::resurrect::resurrect_on_open`.
  - `crates/flux-cli/src/session.rs`: `run_repl` runs `resurrect_on_open` once at startup and
    again on `/resume` (a session switch is also a turn-entry point). Updated the `flux sessions`
    hint string to name all three entry points instead of only one-shot `flux run`.
  - `crates/flux-tui/src/lib.rs`: `run_with_options` runs `resurrect_on_open` (a plain-stderr
    reporter over the same shared step, via a new local `DiscardSink`) before the terminal takes
    over the screen and before `project_session`/`load_history` project the session, so a
    resurrected turn's persisted messages show up in the transcript normally.
  - Docs rolled back: `website/docs/agent/cli.md` ("Crash recovery and resurrection"),
    `website/docs/sdk/agent-lab.md` (the fixture/resurrect paragraph),
    `docs/designs/deterministic-agent-lab.md` (CLI surface section + deviation note #3) all now
    say every turn-entry point resurrects, not just one-shot `flux run`/`Session::send`.
  - Gate: `cargo test -p codewandler-flux-flow` (185 passed, incl. the new tail-guard test),
    `cargo test -p codewandler-flux-sdk --features test-kit` (all green except
    `whatif.rs::off_tape_live_replan_records_both_served_and_live_cells`, owned by a concurrent
    agent's in-flight D-18x work on `whatif.rs`/`cassette.rs` — unrelated to this story, verified
    by isolating the file), `cargo test -p flux-cli` (all green, incl. `website_contract`),
    `cargo test -p flux-tui` (57 passed), `cargo clippy -D warnings` on all four packages clean,
    `cargo fmt --all -- --check` clean on every file this story touches.

## Notes
- The turn guard already serializes entries, so the step composes; the risk to watch is `stream()`'s
  early return shape — resurrection output must be reported through the stream, never silent.
- Depends on nothing; independent of D-181 (turn-scoping) but test them together for the
  crash-mid-stream case.
