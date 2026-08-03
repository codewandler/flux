---
id: C-441
title: "A context-management section — the question every user asks, answered nowhere"
pillar: Core
status: done
priority: 5
design: docs/designs/docs-completeness.md
epic: docs-completeness
areas: [website, docs]
note: "⚠ the mechanism is implemented and its entire user-facing documentation is ONE ROW in a config table: `FLUX_COMPACT_CHARS`. `context-packs.md` (the Flux-Lang ctx construct) and `project-context.md` are real and answer different questions, which is why the gap looks covered from the inside"
---

# What happens when the conversation gets long

## Goal

A user can answer, from the docs: what fills the context, what flux does when it fills, what is lost,
what is kept, how to control it, and what it means for the session afterwards.

## Why it looks covered and is not

- `website/docs/language/context-packs.md` documents `ctx` — the **Flux-Lang** construct.
- `website/docs/agent/project-context.md` documents **project** context.
- `website/docs/reference/config.md:507` documents `FLUX_COMPACT_CHARS` as one row: *"Character
  threshold that triggers history compaction."*

Three real pages, none of which answers *"what happens when my conversation gets long?"* — and their
existence is exactly why the gap is invisible from inside the project.

⚠ **This may be findability as much as coverage.** `FLUX_COMPACT_CHARS` *is* documented — in a
500-line config table, where nobody searching for a concept will meet it. A concept page that links to
the knob is the fix; a second copy of the knob is not.

## Acceptance

- [x] ⚠ **Blocked on [C-443](C-443-zero-compacted-rows.md)** until it is known whether compaction
      actually fires. A page describing behaviour nobody has observed is documentation of an intention,
      and a 112k-event store contains zero `Compacted` rows.
      → unblocked: C-443 is `done` and answered **possibility 1** (armed and correct, threshold rarely
      reached). Its finding is carried into the page's "What flux does not manage" section verbatim in
      substance — including the zero-compactions sweep — rather than being quietly dropped.
- [x] The page answers all six: what fills the context · what happens when it fills · what is lost ·
      what is kept · how to control it · what it means afterwards.
      → `website/docs/agent/context-management.md`, one section per question in that order.
- [x] ⚠ **It says plainly that compaction *replaces* history in the durable log** — `EventKind::Compacted
      { messages }`. That changes what a session is for `flux replay`, `flux export` and anything
      reconstructing a run, and a user who does not know it will be surprised at the worst moment.
      → an admonition ("Compaction replaces the live history") plus a per-reader breakdown. ⚠ The
      breakdown is **more specific than the story assumed**: the log keeps both (append-only), `flux
      replay` is *unaffected* (it reads the plan/cassette events, not the conversation), and `flux
      export` renders the **pre**-compaction turns with no compaction marker. See Progress.
- [x] The relationship to the neighbours is stated, so the three pages stop looking like alternatives:
      Flux-Lang's `ctx`, project context, and this.
      → a three-row "question it answers" table at the top of the new page, plus reciprocal links added
      to `project-context.md` and `language/context-packs.md` so the relation is visible from all three.
- [x] Every knob it names links to the config reference rather than restating it — one source of truth
      for the value, one place for the concept.
      → the control table links every row to `reference/config.md`. Pinned:
      `context_management_page_matches_the_compaction_the_code_implements` reads the two defaults out of
      `DEFAULT_COMPACT_THRESHOLD_CHARS` and `tool_output_cap()` and fails if the page's numbers drift.
- [x] ⚠ Honest about what is *not* managed. If flux does not do something users expect from other
      harnesses — automatic summarization, per-tool budgets, retrieval — say so rather than leaving the
      reader to infer it exists.
      → "What flux does not manage": eight absences, each verified absent in the tree (no tokenizer, no
      `TokenCounter` impl, no transcript retrieval, no per-tool budget, no message cap, no tiered
      compaction, no project-context or skill cap).
- [x] Where the page states behaviour, it is behaviour the code does. If C-443 finds compaction rarely
      fires, the page says so.
      → every behavioural sentence traced to a read line (`compaction_attempt`,
      `SessionLog::rewrite`, `projection::conversation`, `export_cmd::render_session`,
      `replay::replay_session`). "Compaction rarely fires in practice" is stated with C-443's numbers
      *and* its bound ("that measures a workload, not a ceiling").
- [x] Full gate green, including the website checks.
      → all seven dispatched commands green; `cargo test -p flux-cli --test website_contract` 29/29.
      ⚠ The Docusaurus build itself was **not** run — `website/node_modules` is absent in this
      worktree, so `onBrokenLinks: 'throw'` did not execute. Link targets and the one anchor
      (`project-context.md#guidance-fragments`) were verified by hand instead.

## Notes

- The peer-docs audit ([C-442](C-442-peer-docs-gap-audit.md)) will likely surface neighbours worth
  covering in the same section — token budgets, session boundaries. Do not wait for it; do not duplicate
  it either.
- ⚠ The register to match is `vision.md`'s: it states the improvement-loop pillar is *"currently
  aspirational, and this document says so honestly."* A context page that oversells is worse than none,
  because context handling is exactly what an evaluator stress-tests first.

## Progress

- Filed 2026-08-02 at the owner's request.
- 2026-08-02 — implemented as `website/docs/agent/context-management.md`, in the sidebar's "Run the
  coding agent" group directly after `project-context`. Pinned by
  `context_management_page_matches_the_compaction_the_code_implements`
  (`crates/flux-cli/tests/website_contract.rs`), which fails at the merge base (page absent).
- **Findability, not just coverage** (the story's own ⚠). The knob was already documented; nobody could
  find it. So the fix runs both ways: the concept page links *out* to the config reference for values,
  and `reference/config.md` + `troubleshooting.md` now link *in* to the concept. The `FLUX_COMPACT_CHARS`
  row also gained the two facts a one-line row was hiding — `0` disables, and the count is not a
  context-window fraction.
- ⚠ **C-443's suggested wording was corrected on one point before use.** It reads "a replay or export
  can still see what was replaced", which is true of the *log* but not of either command. Verified at
  this tip: `flux replay` never reads the conversation at all (`flux-flow/src/replay.rs:125` reads
  `run_trace` + `plans_by_key`), so compaction cannot affect it; `flux export` renders turns from the
  turn log (`export_cmd.rs:131` → `projection::turns`), so it shows the **pre**-compaction turns and
  carries **no compaction marker**. The page states all three readers separately rather than collapsing
  them. The "nothing is silently dropped" guarantee itself was re-verified independently:
  `SessionLog::rewrite` is the sole `Compacted` writer (`session_log.rs:187-190`) and the sole
  history-replacement path, and `projection::conversation` only `clear()`s the fold.
- ⚠ **C-462 is documented as a limitation, not papered over and not fixed.** The page says outright that
  the threshold is a flat character count that does not consult the model's context window, gives the
  ~12k-token equivalence, and originally called it an open question. C-462 subsequently kept it as an
  intentional fixed history budget and updated the page; the pin still requires the page to say the
  threshold *does not* consult the context window, and separately pins the `messages.len() < 4` floor
  and `keep = 2` so rewording cannot drift from the gates.
- Deliberately **not** written into the page: the summarizer's hard-coded `max_tokens: 1024` and the
  cancellation/empty-summary silent no-ops. Both are real (`engine.rs:1671`, `1686`, `1699`) but are
  internals a user cannot act on; the one user-visible consequence — `/compact` reporting success after
  a no-op — *is* documented. See ADJACENT in the handoff.
- **Six adjacent findings were reported; the four that are defects are now filed** and were deliberately
  left unfixed here, because C-441 was a documentation story:
  [C-465](C-465-compact-claims-success-on-five-no-ops.md) (`/compact` prints "context compacted" for five
  distinct no-ops, including compaction-disabled and user-cancelled),
  [C-466](C-466-compact-threshold-default-drifts.md) (the CLI hard-codes the default twice more, so this
  story's new pin verifies the constant against the page while the CLI can drift from both — plus the
  served path silently ignored a malformed `FLUX_COMPACT_CHARS` where the CLI warned; both parts were
  subsequently closed by C-466 and [C-507](C-507-served-compaction-env-typo-is-silent.md)),
  [C-469](C-469-tokencounter-has-no-production-implementor.md) (the unused `TokenCounter` seams were
  subsequently retired, making the deterministic estimate explicit), and
  [C-468](C-468-plugin-host-test-hard-fails-under-tmpfs-pressure.md) (unrelated area, found in the same
  gate run).
