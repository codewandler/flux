---
id: C-647
title: "Attention is a present condition, not an accumulated history"
pillar: "Core"
status: ready
priority: 5
epic: recovery-and-inspection-have-no-cli-so-every-failure-is-hand-driven
areas: [flux-cli]
design: recovery-and-inspection-have-no-cli-so-every-failure-is-hand-driven
note: "25 of 25 flagged workers are cancelled or failed corpses of waves long finished; the driver halts on attention_required and therefore halts forever"
---

# Attention is a present condition, not an accumulated history

## Goal

A worker that was cancelled two hundred waves ago is a record of something that already happened, not a
condition needing action. Today it counts as present attention forever, and because the driver refuses to
act while anything needs attention, the fleet stops permanently once enough history accumulates.

Observed after one long session: **25 of 25** workers flagged for attention were corpses — 20 ,
3 , 2 already  — from waves that had finished, been reclaimed, or been superseded.
The main agent also carried a stale  from a TUI turn that failed hours earlier and had been
succeeded many times since. The driver logged  and would
have logged it every minute forever.

This is the same shape as two individually-correct rules composing into a stall: "flag anything that needs a
human" and "do not act while something needs a human" are each defensible, and together they guarantee that
a fleet which has ever had a problem can never run unattended again.

## Acceptance

- [ ] Failing first, a test proves that a  or  worker belonging to a wave that is
      terminal does not raise present attention, while the same record in a live wave does.
- [ ] An error recorded on an agent is superseded by a later successful turn on that agent, so
       describes the last outcome rather than the worst one ever seen.
- [ ]  distinguishes *actionable now* from *happened once*: open decisions and live
      failures raise it; history does not.
- [ ] The count an operator sees matches the number of things they can actually do something about, so a
      surface showing "25 need attention" is never mostly archaeology.
