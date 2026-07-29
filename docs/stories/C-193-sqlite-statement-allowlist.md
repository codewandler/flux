---
id: C-193
title: "sqlite_query's SQL admission is a bypassable prefix denylist documented as an allowlist"
pillar: Core
status: ready
priority: 3
epic: security-assurance
design: docs/designs/security-assurance.md
note: "REVIEW — the op description promises 'only SELECT and PRAGMA are allowed' while is_write_sql implements the inverse; a leading /* comment */ defeats it, and the entries it does catch are already covered by SQLITE_OPEN_READ_ONLY"
---

# sqlite_query's SQL admission is a bypassable prefix denylist documented as an allowlist

## Goal
Make the implementation match the contract the op already advertises to the model. Today the
description and the refusal message both promise an allowlist; the code is a ten-keyword prefix
denylist that is bypassable where it matters and redundant where it works.

## Acceptance
- [ ] Failing-first test: `sql` beginning with a comment — e.g. `/*x*/ INSERT INTO t VALUES(1)` —
      is refused by flux's own admission check, not merely by SQLite's read-only flag. This must fail
      against the current tree.
- [ ] Admission is an **allowlist** over the statement type: the first meaningful token after
      comment- and whitespace-stripping must be one of `SELECT` / `WITH` / `PRAGMA` / `EXPLAIN`
      (final set to be decided in the change), and anything else is refused.
- [ ] `VACUUM` is refused by that allowlist as a consequence, not as a special case — this is the
      shared acceptance with [C-192](C-192-sqlite-query-vacuum-into-escape.md).
- [ ] The op description (`extra.rs:269-271`) and the refusal message (`:322`) describe what the
      code now actually does.
- [ ] Test that the allowlist is applied to the statement *as SQLite will parse it*, so comment
      forms, leading whitespace and case cannot separate the two.

## Progress
- (not started)

## Notes
- **Verified against the tree at `0.33.1` (f8e90d7).** Source review:
  [`reviews/2026-07-29-envelope-integrity.md`](../../reviews/2026-07-29-envelope-integrity.md),
  finding 2.
- The claim: `crates/flux-tools/src/extra.rs:269-271` — *"Only SELECT and PRAGMA statements are
  allowed"*; the refusal message at `:322` repeats it.
- The implementation: `:207-218` — `sql.trim_start().to_ascii_uppercase()` then `starts_with` over
  ten keywords. `trim_start()` strips whitespace, **not comments**. Pinned: `"/*x*/ INSERT …"` →
  `starts_with("INSERT")` is `false`, and SQLite parses the comment and executes the statement.
- The denylist is also largely redundant with the connection flag: `SQLITE_OPEN_READ_ONLY`
  (`:343`) already blocks `INSERT`/`UPDATE`/`DELETE`/`DROP`/`ALTER`/`CREATE`/`REPLACE` — verified.
  So the check contributes almost no defense where it works.
- `ATTACH` is the one denylist entry the flag does not cover, but it is bounded in practice:
  `prepare` + `query` executes only the first statement and the connection is dropped per call, so
  an attach cannot be followed by a select in the same invocation. Worth keeping refused anyway —
  an allowlist gives that for free.
- The entry that is missing entirely is `VACUUM`, which is the whole of C-192. **These two stories
  are one change**; they are split because they are separately testable and C-193 is the part that
  prevents the next missed keyword.
