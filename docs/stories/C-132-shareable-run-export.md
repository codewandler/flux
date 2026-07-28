---
id: C-132
title: Shareable run export — flux export <run> → one self-contained HTML file
pillar: Core
status: done
priority:
epic:
design:
note: "render a session/run (plan visuals via flow_render/render_styled, op results redacted per the evidence rules, diffs, cost, timeline) into a single static HTML file for bug reports, PR links, and demos — the read-only sibling of the Time Machine; no server, no viewer app"
---

# Shareable run export — flux export <run> → one self-contained HTML file

## Goal
Turn a recorded run into an artifact you can attach to an issue or PR: `flux export <run>` renders
the plan (via the `flow_render` substrate), redacted op results, diffs, cost, and a timeline into
one self-contained static HTML file. The read-only sibling of the Time Machine verbs.

## Acceptance
- [ ] `flux export <run> -o run.html` produces a single self-contained file (inline assets, no
  network) that renders the plan tree, per-op results, diffs, cost, and timeline — golden test
  over a recorded mock run.
- [ ] All content passes the durable-evidence redaction rules (C-22); a run containing a seeded
  secret exports with the secret redacted — failing-first test.
- [ ] Sub-agent children included (correlation per A-59) with clear nesting.
- [ ] Plan visuals reuse `flux_lang::highlight` / `render_styled` spans — no second renderer.
- [ ] Export is a pure read: no event-store writes, no provider construction.

## Progress
- (not started — filed from the 2026-07-28 feature-suggestion pass)
- 2026-07-29 — Implemented `flux export <run> [-o OUT]` in `crates/flux-cli/src/export_cmd.rs`
  (new module, wired into `args.rs`/`dispatch.rs`/`main.rs`), placed alongside the other Time
  Machine verbs. `<run>` resolves exactly like `replay`/`fork`/`diff` (`last`, or an existing
  session id via `EventStore::info`) and honors `--store` for free (same `open_event_store()` seam).
  Pure read: only `EventStore` getters (`info`/`turns`/`run_trace`/`cost_summary`/`observations`/
  `children_of`) — no append, no provider/agent construction. Output is one static HTML file:
  inline `<style>` (light/dark via `prefers-color-scheme`), no `<script>`, no external refs.
  - Plan tree: parses each turn's accepted `plan_source` (L-38) and renders it through
    `flux_lang::render::render_styled_spans` — the exact substrate `flow_render`'s SVG tree view
    uses — mapping each `(text, Role)` span to a CSS class instead of an SVG fill. No second
    renderer.
  - Per-op results + diffs: from `run_trace`'s `RunEvent::OpRecorded` cells (already redacted at
    record time, C-22/C-43). `write`/`edit`/`patch` ops style their `view` (a unified diff — see
    `flux_tools::unified_diff`/`edit_result`) as a diff block with per-line +/-/hunk/header classes.
  - Cost: `EventStore::cost_summary` per session, one table row per model.
  - Timeline: `EventStore::turns` (prompt, plan attempts, answer, timing) per turn.
  - Sub-agents (A-59/A-08): `children_of` gives the correlated child set; ordered by the parent's
    `subagent.trace` observation (`data.session`, the point the `task` call actually landed),
    falling back to creation order for anything without one; rendered recursively as nested
    `<details>` sections (CSS `.session .session` handles the visual indent — no depth counter is
    threaded through `render_session` at all, since nothing ever read it).
  - Redaction (C-22): `run_trace`/`observations`/`plan_source` are already redacted at record time
    (`RecordScope::record` in `flux-flow/src/cassette.rs`, `flush_observations`), and every one of
    those strings is still run through a **fresh** `Redactor::new()` at export time anyway
    (defense-in-depth, idempotent). Conversation `Message` text / `TurnSummary.user_input`/`answer`
    are the one field C-22 never covered — confirmed `flux-flow/src/engine.rs::begin_turn_lifecycle`
    calls `record_message`/`begin_turn` with the raw prompt, no redactor in the path — so for those
    the export-time `Redactor` is the *only* control, and it's shape-based only (`sk-…`/`ghp_…`/JWT/…).
  - Found and fixed (scoped to this command) a real gap while writing the diff-redaction test: a
    unified-diff `+`/`-` line-prefix defeats `flux_secret::Redactor`'s credential-shape matcher,
    because `+`/`-` aren't token-boundary characters in `redact_patterns` — `+sk-ant-…` reads as one
    non-matching token. Fixed locally in `render_diff` by stripping the marker before redacting the
    line content, so it hits a real boundary. NOT fixed in `flux_secret` itself (broader blast
    radius — affects every live diff view written by `RecordScope::record`, not just export, so it's
    out of this story's scope); flagging it here as a candidate follow-up story rather than touching
    `flux_secret::redact_patterns`'s boundary-character set myself.
  - Failing-first verified by hand: temporarily made `redact_esc` a no-op redaction pass, confirmed
    `seeded_secret_in_conversation_is_redacted_in_the_export` fails with the raw secret visible in
    the rendered prompt field, then restored the real redaction call and confirmed it passes again.
  - Tests: `crates/flux-cli/src/export_cmd.rs` unit tests (in-memory `EventStore`, no binary spawn) —
    `seeded_secret_in_conversation_is_redacted_in_the_export`,
    `exports_a_minimal_session_as_one_self_contained_document`,
    `export_never_writes_to_the_event_store`, `write_op_diff_renders_with_diff_line_styling`,
    `sub_agent_child_is_nested_under_the_parent`. Golden integration test (real binary, `-m mock`
    recording → export → assertions on the written file, incl. no `<script>`/`<link>`/network refs,
    plan-tree/op/cost/timeline markers, and stdout-vs-file byte-identical re-export):
    `crates/flux-cli/tests/export_smoke.rs::export_renders_a_recorded_mock_run_as_one_self_contained_html_file`.
  - Website docs mirror updated (`website/docs/agent/cli.md`): added the `flux export` row to the
    Subcommands table and a `--store` example — required by the existing
    `cli_reference_covers_every_public_subcommand` website-contract test, which failed until this
    was added.
  - Gate (crate-scoped, `flux-cli` only — no workspace-wide command run): `cargo test -p flux-cli`
    240 passed / 0 failed across unit + all 7 integration test binaries; `cargo clippy -p flux-cli
    --all-targets -- -D warnings` clean; `cargo fmt -p flux-cli -- --check` clean. No other crate's
    source was touched (flux-events/flux-tui carry unrelated other-story uncommitted work, left
    alone).
  - Surprise: the scripted `-m mock` provider's "write a quick note" turn resolves to the `append`
    native tool, not `write` — `AppendTool`'s description contains the backtick-quoted substring
    `` `write` ``, which the mock's family-matching `find()` picks up before it reaches the actual
    `write` tool. `append` never calls `unified_diff`/`edit_result` (no `view` at all), so the golden
    test asserts on the `append` op result instead of a diff; diff-block rendering is covered by a
    dedicated unit test that seeds a `write`-shaped `OpRecorded` event directly.
  - Not done: did not tick Acceptance items or flip `status:` (leaving that to the story owner);
    did not touch CHANGELOG.md/WHATS-NEW.md/docs/roadmap.md/docs/stories/README.md.

## Notes
- Deliberately not a web UI or server: a file. Complements the optional TUI cockpit (A-47).
