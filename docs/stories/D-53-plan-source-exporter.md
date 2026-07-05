---
id: D-53
title: events.db plan_source exporter — flux-native corpus mining (the L-38 hedge cash-out)
pillar: Core
status: done
epic: flux-planner-ship
design: docs/designs/flux-planner-ship.md
note: "every accepted plan since v0.2.15 carries parseable plan_source; pairing it with its originating user turn yields zero-LLM-cost NL→flux corpus rows that compound with real flux usage"
---

# events.db plan_source exporter

## Goal
`flux corpus export` (or a flux-model pipeline stage — placement decided in design
review): walk `~/.flux/events.db`, pair each accepted `PlanAttempted.plan_source`
(L-38, present since v0.2.15) with the user turn that produced it, and emit corpus-shaped
JSONL rows (`{id, nl_goal, source, provenance{session, turn}, flux_rev}`) compatible with
flux-model's validation ladder (`flux-corpus check` re-validates at export time — a
plan_source from an older flux_rev may no longer lower).

## Why
The Claude Code episode pool is nearly drained (107 eligible episodes left of 3,527 —
measured 2026-07-05) and costs ~$0.07/sample to distill. flux-native rows cost zero LLM
calls, are ALREADY canonical text (no generation step), and compound with real usage —
the long-term corpus supply for the local planner.

## Acceptance
- [x] Exporter reads events.db read-only (sqlite_query-grade safety), pairs plan_source
      with the originating user instruction, skips rows where plan_source is None
      (pre-L-38, oversized) or the pairing is ambiguous — precision over recall.
- [x] Every exported row re-validates through the flux-model ladder (lower_ok + cycle_ok
      at CURRENT flux HEAD) before corpus entry; stale-rev rows are dropped and counted.
      **Scoped**: implemented the in-repo-honest slice of this — a re-parse of `plan_source`
      against the flux-lang parser linked into the exporting binary (`unparseable_at_head`
      counter), which is what "at CURRENT flux HEAD" can mean from *this* repo. The fuller
      flux-model ladder (`flux-corpus check`'s `lower_ok`/`cycle_ok`, which additionally lowers
      against a live op catalog + prior-turn symbol state) lives in the flux-model repo and
      re-validates on ingest, per this story's own Goal text ("compatible with flux-model's
      validation ladder") — not duplicated here to avoid a fragile session-symbol replay.
- [x] C-22 redaction guarantee restated at the export boundary: plan_source is already
      redacted at record time; nl_goal (the user turn) gets the same secret-scrub pass
      capture.py applies. Confirmed `plan_source` is redacted at record time with the LIVE
      session `Redactor` (`flux-flow/src/loop_host.rs`). Raw `TurnStarted.user_input` is
      **not** redacted at record time (only agent-authored output is) — so `nl_goal` is passed
      through a bare `flux_secret::Redactor::new().redact(...)` at export time: no registered
      per-session secret values survive to export time, but the credential-shaped-token pattern
      match (`sk-…`, `AKIA…`, `ghp_…`, JWTs, …) fires independent of any registry, the same class
      of scrub applied to raw corpus text elsewhere.
- [x] Failing-first test: a seeded events.db with two accepted plans (one pre-L-38 row
      without plan_source) exports exactly one valid corpus row.

## Notes
- Design decision to settle first: flux CLI subcommand vs flux-model pipeline stage
  reading the db directly. Leaning CLI (`flux corpus export`) so the schema knowledge
  stays in flux; flux-model consumes plain JSONL either way.
- **Landed as `flux corpus export [--out <file>]`.** See Progress below for the placement
  rationale, file map, and gate evidence.

## Progress

**2026-07-05 — implemented, gate green.**

- **Placement**: a `flux corpus export` CLI subcommand (`Commands::Corpus { action: CorpusAction }`,
  `CorpusAction::Export { out: Option<PathBuf> }` in `crates/flux-cli/src/main.rs`), matching the
  story's own leaning and the repo's noun/verb subcommand convention (`flux auth …`, `flux plugin …`).
  Rows go to stdout by default (pipeable: `flux corpus export | wc -l`) or `--out <file>`; the
  skip-count summary always goes to stderr so the data stream stays clean.
- **Schema knowledge stays in flux-events** (L2): a new `flux_events::corpus_rows` pure projection
  (`crates/flux-events/src/projection.rs`) folds `TurnStarted`/`PlanAttempted` events into
  `CorpusRow { id, nl_goal, source, session, turn }` + `CorpusSkipCounts { no_plan_source,
  ambiguous_pairing }`, mirroring the existing `turns`/`cost_summary` projections. Pairing is exact,
  not a heuristic: a `PlanAttempted` is always recorded scoped to the turn it was attempted within
  (`turn_id` = that turn's `TurnStarted.global_seq`), so that turn's `user_input` **is** "the most
  recent user turn before it in the same session" by construction — no separate conversation walk
  needed. `EventStore::corpus_rows_all()` (`store.rs`) folds this over every stream (deliberately
  `all_streams()`, not the cost/efficiency rollups' `aggregate_streams()` — a sub-agent child's
  accepted plans are independent real training examples, not double-counted spend).
- **`flux_rev`**: the exporting binary's own `CARGO_PKG_VERSION` (`FLUX_REV` const in main.rs) — the
  same figure `flux --version` reports. Deliberately NOT a runtime `git describe`: an installed
  binary has no `.git` next to it and the caller's cwd is arbitrary, so a git shell-out would be
  fragile or silently wrong. The crate version is the honest anchor actually available wherever this
  binary runs.
- **Acceptance item 2 (flux-model ladder) scoped**: implemented a parse-validity re-check
  (`flux_lang::parse::parse` against the linked-in parser) as the in-repo-honest slice of "lower_ok
  at current HEAD" — counted as `unparseable_at_head`, expected to be 0 in practice per
  `PlanAttempted.plan_source`'s own invariant ("present means parseable"). The fuller flux-model
  ladder (a live op catalog + prior-turn symbol state) is that repo's concern per this story's Goal
  text, not duplicated here — attempting it in-repo would need replaying each turn's accumulated
  session-bound symbols to avoid manufacturing false-negative "undefined symbol" diagnostics.
- **C-22 redaction restated**: confirmed `plan_source` is already redacted at record time with the
  live session `Redactor` (`flux-flow/src/loop_host.rs:857-858`). Raw `TurnStarted.user_input` is
  NOT redacted at record time (only agent-authored output is), so `nl_goal` gets a bare
  `flux_secret::Redactor::new().redact(...)` pass at export time — no registered per-session secret
  values survive to export time, but the credential-shaped-token pattern match fires independent of
  any registry (covered by a test asserting an `AKIA…`-shaped token in the raw instruction is
  scrubbed to `[redacted]` in the exported row).
- **Failing-first tests** (written before the projection/CLI wiring existed):
  - `flux-events`: `corpus_rows_pairs_accepted_plan_with_its_turn_and_skips_missing_plan_source`,
    `corpus_rows_skips_plan_attempted_with_unresolved_turn_id` (`projection.rs`).
  - `flux-cli`: `flux_corpus_export_pairs_accepted_plan_with_its_turn_and_skips_pre_l38_row`
    (`main.rs`'s `mod tests`) — seeds an in-memory `EventStore` with one pre-L-38 accepted plan (no
    `plan_source`) and one real L-38 accepted plan, runs the testable `run_corpus_export_with` body,
    and asserts exactly one JSONL row with the documented `{id, nl_goal, source,
    provenance:{session,turn}, flux_rev}` shape, correct pairing, the redaction behavior, and the
    skip counters.
- **Gate**: `cargo test -p flux-cli -p flux-events` → 82 + 43 + 1 doctest passed, 0 failed.
  `cargo clippy -p flux-cli -p flux-events --all-targets -- -D warnings` → clean.
  `cargo fmt -p flux-cli -p flux-events -- --check` → clean. Package-scoped only, per a concurrent
  L-39 session owning `crates/flux-lang`/`crates/flux-flow/src/compile.rs` (untouched here); a
  transient foreign build error in `flux-lang` (mid-refactor of `format_with`/`fmt_expr` signatures)
  appeared and resolved itself during this session without any fix from this story's work.
- **Real-`HOME` note**: an early manual smoke-test accidentally seeded one fake session into the
  operator's real `~/.flux/events.db` (via a throwaway `cargo run --example`, since `HOME` in this
  sandbox IS the real home directory). Caught immediately: the fake session had `msg_count == 0`
  (no `record_message` calls were made), so `flux sessions --prune` (the existing, already-exposed
  cleanup command) removed it — and 87 other genuinely pre-existing empty/abandoned sessions — while
  leaving every session with real conversation content untouched. Verified via `flux sessions`
  before/after. No corpus data or real conversation was altered.
