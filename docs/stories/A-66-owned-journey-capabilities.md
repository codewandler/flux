---
id: A-66
title: Make app journeys inherit agent context and capability ceilings
pillar: Agent
status: done
design: docs/designs/owned-journeys.md
note: Turn the tutorial's unreliable agent retrieval into a deterministic, agent-owned journey.
---

# Make app journeys inherit agent context and capability ceilings

## Goal
Make `journey … agent <name>` a real runtime ownership boundary: deterministic journeys inherit the
agent's model, persona, datasource scope, and app-narrowed capabilities. Teach the distinction in the
public tutorial by refactoring an intentionally unreliable agent turn into an explicit retrieval flow.

## Acceptance
- [x] `program_permissions_and_agent_narrowing_parse` fails first, then proves typed app/agent
      capability declarations parse with inherit-vs-empty semantics.
- [x] `owned_journey_inherits_model_persona_datasource_and_capabilities` fails first, then proves an
      owned journey must search the declared datasource before its owner-model reasoning call.
- [x] `app_capability_ceiling_is_absolute_under_auto_approve` proves `--yes` cannot widen app code.
- [x] `host_permission_rules_apply_inside_but_never_widen_source_ceiling` proves layered local
      permission rules apply inside the app ceiling and local denies win.
- [x] Fallible app construction rejects unknown owners, datasources, operations, tools, and calls
      outside the effective app/agent ceiling before channels start.
- [x] The public tutorial preserves the current model-controlled failure as Part A, then makes the
      same example reliable with an owned journey in Part B; executable website contracts cover it.
- [x] Changelogs, language/app references, and the full repository gate are green.

## Progress
- 2026-07-13: User approved the layered app-ceiling + agent-narrowing design and asked for implementation.
- 2026-07-13: Shipped syntax/runtime/tutorial contracts; full workspace gate, website build, and all
  three editor grammar mirrors are green. No commits made (not requested).

## Notes
- `tools` remains the model-visible catalog; `allow`/`deny` govern authored runtime calls.
- Capability entries are exact operation names. Subject-scoped approval rules remain local config.
