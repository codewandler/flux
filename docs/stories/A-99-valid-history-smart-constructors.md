---
id: A-99
title: "ValidHistory + AssistantMessage smart constructors — the session-shape rules as types"
pillar: Agent
status: done
epic: typed-session-log
design: docs/designs/typed-session-log.md
note: "the shape rules become constructors that reject invalid input, unit-tested standalone before any call site moves; absorbs compaction's ad-hoc has_tool_result snapping (engine.rs:1629) into the type"
---

# ValidHistory + AssistantMessage smart constructors — the session-shape rules as types

## Goal
Put the three session-shape invariants into `flux-events` as constructors that cannot produce an
invalid value: an empty assistant message, a split `tool_use`/`tool_result` pair, and a broken role
alternation all become `Err` at construction rather than a provider 400 at request time. This is
the foundation the typed log handle (A-100) is built on, and it lands with no call-site changes so
it can be reviewed and tested in isolation.

## Acceptance
- [x] `AssistantMessage::new(blocks)` rejects an empty block list and a lone all-whitespace text
      block; there is no public path to construct one that violates this. Unit-tested both ways.
- [x] `ValidHistory::try_from(Vec<Message>)` accepts a sequence iff it satisfies all three
      invariants, rejecting: an orphaned `tool_use`, an orphaned `tool_result`, `user`-after-`user`,
      `assistant`-after-`assistant`, and an empty assistant message anywhere in the sequence.
- [x] The rejection carries a `ShapeError` naming which invariant failed and the offending index —
      a caller must be able to log something better than "invalid".
- [x] A property test over generated message sequences: `try_from` succeeds exactly when a
      reference predicate says the sequence is valid.
- [x] Compaction's boundary-snapping rule (currently `has_tool_result`, `flux-flow/engine.rs:1629`)
      is expressible through this type — a `ValidHistory::snap(messages, keep)` helper that returns
      the largest valid suffix-preserving split. Unit-tested against the case compaction guards
      today (a `tool_result` at the split point walks the boundary back).
- [x] No call sites change in this story; `record_message`/`record_compaction` still exist.

## Progress
- 2026-07-29 — **DONE.** `crates/flux-events/src/shape.rs` (new module, exported from `lib.rs`):
  `ShapeError`, `AssistantMessage`, `ValidHistory`, `ValidHistory::snap`. 21 new tests; the crate's
  suite goes 77 → 98, all green. `rustfmt --check` clean; `clippy --all-targets -D warnings` clean.
  No call sites changed, `record_message`/`record_compaction` still exist — as scoped.
- **Found a live bug while writing `snap`** (see Notes) — the inline compaction walk-back guards
  only one of the two ways a suffix can be unsplittable. `snap` guards both, and
  `snap_walks_back_off_a_user_boundary` pins it. The *fix* lands when compaction moves onto
  `rewrite` in **A-101**.
- Deviation from the acceptance list, deliberate: two invariants beyond the three named were added
  because the validator would otherwise accept histories no provider will —
  `MustStartWithUser` and `SystemInHistory`. Both are recorded as their own `ShapeError` variants.
- The property test is an exhaustive sweep over all sequences of length 1..=3 drawn from a
  six-message alphabet (plain user/assistant, empty assistant, tool_use, tool_result, system),
  cross-checked against an independently written reference predicate — rather than a randomized
  generator, so it is deterministic and needs no proptest dependency.

## Findings
- **Compaction can currently produce `user`-after-`user`.** The persisted conversation is a strict
  `user, assistant, …` alternation (only `engine.rs:422` and `:1177` write it), so with
  `keep = 2` the split index `len - keep` is always **even — a `user` message**.
  `has_tool_result` (`engine.rs:1629`) returns false for a plain user text message, so the
  walk-back loop does not move, and `new_msgs = [user_summary] + [user, assistant]`
  (`engine.rs:~1508`). That is `user`-after-`user`, written straight to the log via
  `record_compaction`. Reachable whenever `total > compact_threshold_chars` and `len >= 4`.
  `ValidHistory::snap` returns the correct split (walks back onto the assistant message); wiring it
  in is A-101's job.

## Notes
- Design: [typed-session-log.md](../designs/typed-session-log.md).
- Lives in `flux-events` (beside `projection::conversation`, `projection.rs:19`) because that is the
  crate every writer already depends on; putting it in `flux-core` would drag shape policy into the
  message type itself, which the wire codecs also use for transient (legitimately partial) histories.
- Do **not** apply these rules to the transient per-call histories `staged.rs` builds for model
  stages — those are constructed and consumed in one place and legitimately hold in-flight tool
  pairs. This type governs the *persisted* log only.
