---
id: D-183
title: Run the auto-resurrect step on every turn entry — SDK stream/start_flow and the CLI REPL/TUI
pillar: Agent
status: ready
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
- [ ] `Session::stream` and `Session::start_flow` run the same `auto_resurrect_step` as
      `send`/`send_with` (same turn-guard, same `TurnOutput::resurrected` reporting or the stream
      equivalent).
- [ ] Failing-first test: crash a durable session, resume via `stream()` → the interrupted turn is
      finished first and a later `send()` finds nothing to resurrect (no out-of-order resurrect).
- [ ] The CLI REPL and the TUI run the resurrect-on-open step when entering a session with an
      interrupted turn (same loud reporting, same `FLUX_AUTO_RESURRECT=0` opt-out).
- [ ] Docs updated back: the scoping caveats added on 2026-07-28 to `website/docs/agent/cli.md`,
      `website/docs/sdk/agent-lab.md`, `docs/designs/deterministic-agent-lab.md`, and the
      `flux sessions` hint ("one-shot `flux run` turn") are removed once every entry point
      resurrects.
- [ ] `resurrect::interrupted` additionally refuses (or warns loudly) when the interrupted turn is
      not the session's most recent turn — the out-of-order tail-guard even if an entry point is
      missed again.

## Progress
- (not started)

## Notes
- The turn guard already serializes entries, so the step composes; the risk to watch is `stream()`'s
  early return shape — resurrection output must be reported through the stream, never silent.
- Depends on nothing; independent of D-181 (turn-scoping) but test them together for the
  crash-mid-stream case.
