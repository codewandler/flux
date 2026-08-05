---
id: C-547
title: "One versioned agent CLI contract for `flux board` and `flux fleet`"
pillar: Core
status: ready
priority: 40
epic: first-class-board
design: docs/designs/native-board-fleet-cli.md
areas: [flux-cli, flux-core, flux-markdown]
note: "Decision 0010 foundation — human rendering and flux.cli/v1 JSON are two views of one typed command result; board/fleet skill output is generated and golden-tested"
---

# One versioned agent CLI contract for `flux board` and `flux fleet`

## Goal

Give Claude, Codex and shell automation a stable non-interactive contract before the board and fleet
command families grow. Agents must not parse prose or recreate private Python/socket clients.

## Acceptance

- [ ] Failing-first CLI tests pin the `flux.cli/v1` success and failure envelopes, deterministic
      ordering and clean separation of JSON stdout from diagnostics; the commands do not exist at
      the merge base.
- [ ] Shared CLI plumbing supports `--output human|json|ndjson`, `--request FILE|-`,
      `--idempotency-key`, `--if-revision` and `--dry-run`; unsupported combinations return typed
      schema errors rather than being ignored.
- [ ] Exit classes distinguish input/schema, not-found, conflict/precondition, permission,
      transient worker and validation/gate failures. Public tests pin both code and error body.
- [ ] A durable idempotency record makes a repeated mutating request return its original result, and
      optimistic revision tests prove a stale caller cannot overwrite a newer board/fleet state.
- [ ] `board call`/`fleet call` and `board schema`/`fleet schema` share the exact operation schemas
      used by ergonomic commands; schema fixtures fail when a public command drifts.
- [ ] `board skill` and `fleet skill` use concise in-tree templates, render valid Agent Skill
      frontmatter plus instructions in Markdown, return the same content structurally in JSON, and
      name only commands present in the installed catalogue. Golden tests execute every example.
- [ ] Sensitive fields and paths are redacted before entering any human, JSON or NDJSON result.
- [ ] Website automation documentation and CLI help state that JSON, not human prose, is the agent
      API; user-visible release notes are updated.
- [ ] Targeted CLI and Markdown/skill tests pass; the final board wave owns the full repository gate.

## Notes

- Keep the rendered skills short. Detailed operations belong in `schema`, not copied reference prose.
- This story provides transport and rendering primitives; later stories register actual board/fleet
  operations against them.
