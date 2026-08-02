---
id: C-465
title: "`/compact` reports \"context compacted\" on five distinct no-ops"
pillar: Core
status: ready
priority: 5
areas: [flux-cli, flux-flow]
note: "spun out of C-441: maybe_compact returns Ok(()) for five paths that compact nothing; the CLI prints success for all of them. The TUI already hedges correctly, so the fix has a model in-tree"
---

# A command that asserts something false

## Goal

Make the REPL's `/compact` say what actually happened, so a user who runs it and is told "context
compacted" can trust that the context was compacted.

## The finding

`crates/flux-cli/src/session.rs:987`:

```rust
match agent.maybe_compact(&session_id, &mut sink, &cancel).await {
    Ok(()) => eprintln!("{}", style::dim("context compacted")),
    Err(e) => eprintln!("{} {e}", style::red("compact error:")),
}
```

`Ok(())` is not "compacted". `compaction_attempt` (`crates/flux-flow/src/engine.rs:1624`) returns
`Ok(())` from **five** places, and only the last one did any work:

| `engine.rs` | condition | what happened |
|---|---|---|
| `:1631` | `compact_threshold_chars == 0` | compaction is **disabled** |
| `:1638` | `messages.len() < 4` | session too short |
| `:1645` | `total <= compact_threshold_chars` | **under the threshold — the common case** |
| `:1654` | `ValidHistory::snap` returns `None` | nothing summarizable without breaking history shape |
| `:1686` | `cancel.cancelled()` | the user **interrupted** it |
| end of fn | — | a summary was written |

So a user on a fresh session runs `/compact`, is told "compacting context…" then "context compacted",
and nothing was compacted. The `0`-disables case is the worst of the five: the operator has explicitly
turned compaction off, and the CLI reports it as having run.

⚠ **The TUI already gets this right.** `crates/flux-tui/src/lib.rs:2867` renders `◇ context compacted`
only off the observed compaction event, not off `Ok(())` — and `crates/flux-cli/src/rendering.rs:866`
prints `⊙ context compacted ({from} → {to} messages)` with real counts. Two of three surfaces are
honest; the REPL command is the one that guesses.

## Acceptance

- [ ] A failing-first test: `/compact` on a session under the threshold does **not** claim the context
      was compacted.
- [ ] `maybe_compact`'s result distinguishes "compacted" from "nothing to do" — the caller cannot tell
      today, and no amount of CLI-side wording fixes that without a signal to read.
- [ ] The disabled case (`compact_threshold_chars == 0`) says so distinctly: an operator who turned
      compaction off should be told it is off, not told it ran.
- [ ] The cancelled case does not read as success.
- [ ] ⚠ The existing `Err` branch and the `0`-disables *behaviour* are unchanged — this story changes
      what is **reported**, never whether compaction fires.

## Notes

- The natural shape is for `maybe_compact` to return the outcome (e.g. an enum, or the
  `from`/`to` message counts already available at the write site) rather than `()`. That is a
  signature change on `flux-flow`, which is published as `codewandler-flux-flow` — check whether any
  other caller depends on the current signature before widening it, and treat it as a version
  decision.
- `rendering.rs:866` already has the `{from} → {to}` wording the REPL could reuse once it has the
  numbers, so the user-visible vocabulary need not be invented.
- Related: [C-441](C-441-context-management-doc.md) documents `/compact` for users; if the wording
  changes, the page changes with it. [C-466](C-466-compact-threshold-default-drifts.md) is the other
  defect from the same review.
- Filed 2026-08-02 out of C-441's review; deliberately left unfixed there because C-441 was a
  documentation story and this is a behaviour change.
