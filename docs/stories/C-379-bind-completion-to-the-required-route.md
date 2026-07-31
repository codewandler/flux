---
id: C-379
title: Bind completion to the route the user required
pillar: Agent
status: backlog
epic: harness-route-integrity
design: docs/designs/harness-route-integrity.md
note: "required_route / substitution_allowed and every synonym return ZERO source hits; ExecutionReport carries {id, op, status, result} with no route identity and agent-loop.flux never compares receipts against the declaration. The only guard is one sentence of prompt prose"
---

# Bind completion to the route the user required

## Goal

Make "do it via this mechanism" a checkable contract, so reaching an equivalent end state by other
means is either an explicit substitution decision or a blocked outcome — never a reported success.

## Acceptance

- [ ] `declare_intent` and `IntentDeclaration` (`crates/flux-flow/src/staged.rs:187-201`,
      `:1622-1675`) carry `required_route: Option<String>` and `substitution_allowed: bool`
      (default `true`).
- [ ] At finalize, if `required_route` is set and no `ActionResult` names it, the turn reaches a
      blocked or partial terminal kind rather than a `chat` answer claiming completion.
- [ ] Failing-first: a scripted provider declares `required_route: "flow_run"` and finalizes having
      executed only `git_commit` — `present_results` must receive a non-success terminal kind; the
      success path passes when `flow_run` is in the report.
- [ ] Depends on C-376's route receipt: without a flow identity in the result there is nothing to
      match.
- [ ] The terminal kind is visible to automation clients — coordinate with C-373.

## Progress

- 2026-08-01 — filed from validation of HAR-02. Validation established the mechanism is absent; it
  did not and could not re-verify the specific reported session.

## Notes

- This is the "stop or ask before substituting" half. HAR-01 (missing capability) is C-376/C-377.
