---
id: C-52
title: Op-catalog naming and doc cleanup — mark_read hyphen, flow_render docs gap, dead cognition ops
pillar: Core
status: done
priority:
design:
epic:
areas: [flux-tools, plugins]
note: "three small independent cleanups surfaced by a 2026-07-11 op-naming convention audit; see D-163 for the higher-value web_fetch/web_search rename from the same audit"
---

# Op-catalog naming and doc cleanup — mark_read hyphen, flow_render docs gap, dead cognition ops

## Goal
Three small, independent op-catalog cleanups found while auditing operation naming conventions
across the whole workspace. None are urgent; batch whenever convenient.

## Acceptance
- [ ] `slack.channel.mark-read` → `slack.channel.mark_read` in `plugins/slack/src/main.rs:442` —
      the only hyphenated op name found among 245+ plugin ops; every other multi-word leaf segment
      in every plugin uses an underscore.
- [ ] `flow_render` (`crates/flux-tools/src/render.rs:282`, shipped v0.13.2) documented in
      `crates/flux-flow/docs/ops-reference.md` and `website/docs/language/ops.md` — currently
      missing from both canonical ops docs despite being a live, shipped op.
- [ ] Dead `DedupeTool`/`SortTool`/`FilterTool` structs removed from
      `crates/flux-tools/src/cognition.rs` — never registered by `register_cognition` (the live
      `dedupe`/`sort`/`filter` ops are separate structs in `transform.rs`); harmless today but a
      latent name collision if anyone wires them up by mistake.

## Progress
- 2026-07-11 — Filed from an op-naming consistency audit across core + all 20 plugins.

## Notes
- No functional risk in any of the three; `mark-read`→`mark_read` is a breaking rename to a
  shipped plugin op (slack, v0.14.7) but touches one string with no known external dependents.
