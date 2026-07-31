---
id: C-215
title: "The harness datasource — search(query, harness) over transcripts, contained by construction"
pillar: Core
status: in-progress
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
- [x] Records project as designed: `source: "harness"`, entities `harness.message` and
      `harness.session`, id `<harness>/<session-id>/<index>` stable across re-scans, `title` carrying
      harness + workspace + timestamp, `meta` carrying `{harness, session_id, role, model, workspace,
      ts_ms, path}`, and a message→session link.
      → `datasource/harness_history.rs` `project_message`/`SessionEnvelope::project`; pinned by
      `records_project_as_designed_and_ids_are_stable_across_a_rescan`.
- [x] `search` gains an explicit `harness` field (`flux | codex | claude-code | opencode`; omitted =
      all) lowering onto the record filter. **Failing-first**: a search with `harness: "opencode"`
      against a fixture holding messages from two harnesses returns only opencode's.
      → `datasource/ops.rs` `SearchOp`; pinned by
      `search_with_a_harness_selector_returns_only_that_harnesss_messages`.
- [x] **Off unless explicitly enabled.** The datasource does not register without opt-in.
      **Failing-first, and it is the sharpest test in the epic**: with the feature disabled, a search
      performs **zero** filesystem reads outside the workspace — asserted by observation (no
      candidate root is opened), not by checking the result set is empty. An "off" that still stats
      `~/.claude/projects` is not off.
      → `HarnessIngestReport::roots_opened` (`open_root` records and opens in the same call); pinned
      by `a_disabled_harness_datasource_opens_no_candidate_root` and
      `the_default_pack_advertises_no_harness_selector`. C-216 extends it to every discovery branch.
- [x] **Every body is escaped at ingest**, exactly as A-21 escapes `<knowledge-base>` block bodies.
      Failing-first: a transcript message containing a literal `</knowledge-base>` and an
      instruction-shaped payload cannot break out of its block.
      → `contain()` over the newly-public `flux_core::escape_knowledge_base_body` (A-21's own
      escaper, exported rather than reimplemented); pinned by `every_body_is_escaped_at_ingest`.
- [x] **Every body passes the shared redactor at ingest, not at render.** Failing-first: a fixture
      transcript containing a credential-shaped token is stored redacted, so no later consumer can
      reintroduce it by rendering a different way.
      → `contain()` redacts before it escapes; asserted against the record **in the index**, not a
      rendered result, by `every_body_is_redacted_at_ingest`.
- [x] The op declares a **per-harness permission subject** (e.g. `datasource:harness.opencode`) so a
      policy can allow `flux` and deny the rest, and the declared `ToolSpec` is coherent under
      `flux_spec::metadata_violations` — including its `semantic_effects`, per C-210.
      → `HarnessSelector::subjects`; pinned by
      `the_search_op_declares_a_per_harness_permission_subject`, which also runs
      `metadata_violations(&spec, &search.semantic_effects())`.
- [x] Standard gate green in both workspaces. → `plugins/` untouched, so its `fmt --check` is
      unaffected; the root workspace gate is green (build, test, clippy `-D warnings`, fmt, codegate).

## Progress
- 2026-07-29 — filed with the epic. Depends on C-214; do not start before it lands.
- 2026-07-31 — landed on the **datasource seam**, not a new builtin: `search` is already a
  datasource-pack op and the ask is a filter on it, so nothing was added to `register_builtins` and
  the builtin catalog is unchanged. Both ops-reference mirrors updated for the new field.
  Containment is one function (`contain` = redact → escape) at the ingest seam, so "did we forget to
  redact here?" has exactly one place to be answered. `datasource_tools` *is*
  `datasource_tools_with_history(HarnessHistory::disabled())`, so the off case is one declaration
  rather than two kept in step. **Two follow-ups, both deliberate and recorded in the design doc:**
  the flux-native adapter is C-302 (an enabled `flux` opens no root and is reported as
  `unsupported()`), and the `harness` filter is a post-filter with a bounded 8× over-fetch because no
  index backend filters on `meta` — pushing a `meta` predicate into `DatasourceBackend` would touch
  all four backends and is not this story's blast radius.
- 2026-07-31 — one cross-crate edit outside `flux-capabilities`: `flux_core::context`'s A-21 escaper
  is now public as `escape_knowledge_base_body` (purely additive). Reimplementing it here was the
  alternative and was rejected — a second escaping scheme that can drift from A-21's is exactly what
  this story is supposed to prevent.

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
