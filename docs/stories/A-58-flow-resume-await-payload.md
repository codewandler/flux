---
id: A-58
title: `flow run --resume` must bind the top-level `await` payload (or reject cleanly)
pillar: Agent
status: done
epic: beta-hardening
design: docs/designs/beta-hardening.md
note: "F-015 (beta rec #2): top-level await halts with kind:\"awaiting\", but --resume has no payload option — resume fast-forwards PAST the await, runs post-await work, then dies with `unbound symbol $reply`; either bind a resume payload or refuse to advance"
---

# `flow run --resume` must bind the top-level `await` payload

## Goal
A top-level `await` (e.g. `$reply = ask(...)` lowered to `await`, or a bare top-level `await`) halts
the flow cleanly with `kind:"awaiting"`. But `flux flow run --resume` offers no way to supply the
awaited value, so resume advances *past* the await, runs post-await statements, and then fails with
`unbound symbol $reply`. Resume must either bind a caller-supplied payload to the awaited symbol, or
refuse to advance past an unbound await with a clear error — never silently fast-forward into an
unbound-symbol failure.

## Why (evidence)
- Beta F-015: "Top-level await halted cleanly with `kind:"awaiting"`, but the advertised `--resume`
  command has no payload option. Resume advanced past the await, ran post-await work, and then
  failed with `unbound symbol $reply`."
- The suspend/resume seam already exists (see [A-11](A-11-journey-reply-parking.md), which parks a
  journey `ask` on this exact seam) — the gap is the *CLI* resume verb having no payload input.

## Acceptance
- [ ] `flow run --resume` accepts a payload for the awaited symbol (e.g. `--resume-value <json>` /
      `--reply <…>`, or a documented stdin form), binds it, and post-await work runs with the value
      bound.
- [ ] If no payload is supplied for a suspension that awaits a value, resume **refuses** with a
      clear diagnostic (names the unbound awaited symbol) instead of running into `unbound symbol`.
- [ ] Failing-first test: a flow that halts on `$reply = await …` resumes with a supplied value and
      completes with `$reply` bound; and a resume with no value produces the clear refusal, not an
      `unbound symbol` panic/error.
- [ ] `flow run --resume` help text documents the payload option; the resumable-flow docs show it.

## Progress
- 2026-07-08 **DONE.** Added `flow run --resume-value <json>` (parsed as JSON, so a bare word is a
  string, `42`/`true`/`{…}` keep their type). New flux-lang helpers `bind_resume_value` /
  `awaited_binding` (`runtime.rs`) pre-bind the coerced payload to the halted `await`'s symbol
  *before* the resumable run fast-forwards past it — so the awaited symbol is bound instead of
  failing later on `unbound symbol`. The CLI refuses a value-less resume of a value-await, naming the
  symbol. Threaded `resume_value` through `run_flow` → `run_draft_ast_with_composites_resumable`.
  Tests: `resumable_binds_a_supplied_await_value_and_completes` +
  `awaited_binding_reports_the_symbol_a_resume_must_supply` (flux-lang).

## Notes
- Beta rec order #2.
- Confirm the suspension record already carries the awaited symbol name (the parking key); if not,
  thread it so resume can bind by name.
- Relevant: `crates/flux-cli` flow-run/resume path; the suspension/resume seam in flux-flow.
- Epic: [beta-hardening](../designs/beta-hardening.md).
