---
id: C-215
title: "The harness datasource — search(query, harness) over transcripts, contained by construction"
pillar: Core
status: ready
priority: 12
epic: harness-history
design: docs/designs/harness-history.md
note: "the story that exposes the data is the story that must contain it — off by default, escaped like A-21, redacted at ingest, per-harness permission subject; shipping the datasource without these would make the unsafe version the shipped version"
---

# The harness datasource — `search(query, harness)` over transcripts, contained by construction

## Goal
Project `HarnessMessage` onto datasource `Record`s, register `harness` as a source, and give `search`
a `harness` selector so `search(query: "why did we drop the retry wrapper", harness: "opencode")`
returns the message that answers it — addressable back to its harness, session, workspace and
timestamp.

And contain it in the same change. This is the first datasource whose input is **outside the
workspace jail** (every project the user has ever run that harness in), **secret-bearing by
construction** (transcripts are where credentials get pasted), and **verbatim adversarial text** an
attacker can pre-load once and have retrieved forever. Those three properties all land on the same
`<knowledge-base>` block in the system prompt.

## Acceptance
- [ ] Records project as designed: `source: "harness"`, entities `harness.message` and
      `harness.session`, id `<harness>/<session-id>/<index>` stable across re-scans, `title` carrying
      harness + workspace + timestamp, `meta` carrying `{harness, session_id, role, model, workspace,
      ts_ms, path}`, and a message→session link.
- [ ] `search` gains an explicit `harness` field (`flux | codex | claude-code | opencode`; omitted =
      all) lowering onto the record filter. **Failing-first**: a search with `harness: "opencode"`
      against a fixture holding messages from two harnesses returns only opencode's.
- [ ] **Off unless explicitly enabled.** The datasource does not register without opt-in.
      **Failing-first, and it is the sharpest test in the epic**: with the feature disabled, a search
      performs **zero** filesystem reads outside the workspace — asserted by observation (no
      candidate root is opened), not by checking the result set is empty. An "off" that still stats
      `~/.claude/projects` is not off.
- [ ] **Every body is escaped at ingest**, exactly as A-21 escapes `<knowledge-base>` block bodies.
      Failing-first: a transcript message containing a literal `</knowledge-base>` and an
      instruction-shaped payload cannot break out of its block.
- [ ] **Every body passes the shared redactor at ingest, not at render.** Failing-first: a fixture
      transcript containing a credential-shaped token is stored redacted, so no later consumer can
      reintroduce it by rendering a different way.
- [ ] The op declares a **per-harness permission subject** (e.g. `datasource:harness.opencode`) so a
      policy can allow `flux` and deny the rest, and the declared `ToolSpec` is coherent under
      `flux_spec::metadata_violations` — including its `semantic_effects`, per C-210.
- [ ] Standard gate green in both workspaces.

## Progress
- 2026-07-29 — filed with the epic. Depends on C-214; do not start before it lands.

## Notes
- **Why containment is in this story and not deferred.** Splitting "ship it" from "make it safe"
  creates a window where the unsafe version is the shipped version, and this epic's entire risk lives
  in that window. C-216 hardens what this story must already establish.
- **`harness` is an explicit field, not a `source:` filter, deliberately.** `source: "harness"` cannot
  select *within* the source, and `harness=opencode` is what a user will actually type. Requiring
  them to know the record schema to filter by harness is the wrong surface.
- Redaction at **ingest** rather than render is the C-195 lesson applied in the opposite direction:
  the approval sheet does not redact because it is a human-eyes surface with nothing downstream, and
  this is the mirror case — a persisted index with *everything* downstream.
- The reachability question C-210 just settled applies to the new op's declaration: decide its risk
  tier and its `semantic_effects` honestly, and check the pair with `metadata_violations` rather than
  eyeballing it.
- Keyword search only — no embeddings in this epic (see the design doc's non-goals).
