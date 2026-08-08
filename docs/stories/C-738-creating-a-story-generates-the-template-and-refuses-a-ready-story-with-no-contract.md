---
id: C-738
title: "Creating a story generates the template and refuses a ready story with no contract"
pillar: "Core"
status: backlog
priority: 2
epic: delivery-is-verified
areas: [flux-cli]
---

# Creating a story generates the template and refuses a ready story with no contract

## Goal

`create_item` writes a hardcoded body that does not match `docs/stories/_TEMPLATE.md` — no
`## Progress`, no `## Notes`, an empty `## Goal`, and `- [ ] Define acceptance.` as the sole
criterion. The template exists and is ignored. A story can also be created directly at `ready`,
which makes it dispatchable while its contract says "Define acceptance."

## Acceptance

- [x] A new planning document is generated from `docs/stories/_TEMPLATE.md`, so the template is the
      one definition of a story's shape rather than a second one that drifts.
- [x] `create --status ready` refuses unless the story has a real Goal and at least one criterion —
      a story cannot be born dispatchable and empty.
- [x] Creating at `backlog` still allows a placeholder, because drafting has to be possible; the
      `backlog -> ready` transition is where the contract becomes mandatory.
- [x] Regression test: `create --status ready` with no contract is refused, `create` at backlog
      produces a body matching the template, and the template and generated body cannot diverge
      without a test failing.

## Progress

- Done. `create_item` renders the template instead of a format string: the board's own
  `docs/stories/_TEMPLATE.md` when it ships one, else the copy `board_fleet_cmd.rs` embeds from this
  repository with `include_str!`. A section added to the template appears in the next story with no
  code change, which the regression test proves by adding one.
- `--status ready` is refused unless the *resulting document* has a Goal and at least one criterion.
  The yardstick for "placeholder" is the template itself — whatever it writes under a heading is
  prompt text addressed to the author — so the rule needs no list of literals to keep in sync, and
  `- [ ] Define acceptance.` can no longer be generated at all.
- `--goal` and `--criterion` were added so a ready story can be authored complete in one call.
  Without them the refusal would have been unconditional, which is not what "refuses *unless* the
  story has a real Goal" says.
- Test: `creation_generates_the_template_and_refuses_a_ready_story_with_no_contract` in
  `crates/flux-cli/tests/board_fleet_cli.rs`.

## Notes

- The `backlog -> ready` transition check is C-736's, not this story's; this closes the other door
  into the ready pool.
