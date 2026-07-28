---
id: C-127
title: "Remote approvals — the approval gate over Slack / webhook (epic)"
pillar: Core
status: backlog
priority:
epic: remote-approvals
design:
note: "EPIC — pluggable approver transport for headless/serve agents: approval requests post to Slack (Block Kit buttons) or a signed webhook, timeout = deny, decision lands in the audit trail; complements quorum approval (C-96) — that changes how many approvers, this changes where they are"
---

# Remote approvals — the approval gate over Slack / webhook (epic)

## Goal
Headless `flux app run` / serve-mode agents currently need a terminal for the approval modal. Add a
pluggable approver transport: approval requests post to Slack (Block Kit buttons — the adapter and
default-on slack channel already exist) or a signed webhook, with timeout → deny-by-default, and
the decision recorded in the audit trail like any interactive approval.

## Acceptance
- [ ] An approver-transport seam behind the existing approval gate; the interactive TUI/CLI
  approvers become one implementation of it (no behavior change for them — behavior-lock test).
- [ ] Slack transport: a pending approval renders as a Block Kit message with approve/deny buttons;
  the button decision resolves the gate; hermetic test against a stub Slack surface.
- [ ] Webhook transport: signed request out, verified signed decision in; replayed/forged decisions
  rejected — failing-first test.
- [ ] Timeout → deny-by-default, with the timeout denial audited distinctly from an explicit deny.
- [ ] Every remote decision lands in the audit trail with the deciding principal's identity.

## Progress
- (not started — filed from the 2026-07-28 feature-suggestion pass)

## Notes
- Builds on `flux-channels` + the existing PromptGate seam (see C-91).
- Complements quorum approval (C-96): compose so N remote approvers satisfy a quorum policy.
