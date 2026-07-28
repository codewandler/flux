---
id: C-164
title: Session search — find a past session by content, file, or date
pillar: Core
status: backlog
priority:
epic:
design:
note: "`flux sessions` takes exactly ONE flag, --prune (args.rs Sessions variant), and the store's only query is list(limit)/list_for_account(account, limit) (flux-events/src/store/mod.rs:149-150,294-301) — so finding the session where you touched a given file means scrolling newest-first; every input needed is already in events.db"
---

# Session search — find a past session by content, file, or date

## Goal
Make past work findable. `/resume`, `flux replay`, `flux fork`, and `flux diff` all take a session
id — and the only way to *get* that id is to scroll a newest-first list. Everything needed to search
is already durably in `events.db`; nothing here needs a new store, a new index format, or a network.

## Acceptance
- [ ] `flux sessions` accepts filters — at minimum a free-text query over session content, a
      `--file <path>` filter (sessions that touched that path), and a date range — with results
      still rendered newest-first. Failing-first test over a seeded multi-session store asserting
      each filter selects the right sessions and excludes the others.
- [ ] The query is a **projection over the existing event log**, consistent with the
      event-store-unification canon — no second source of truth and no new persisted index unless a
      measured need justifies one.
- [ ] Both backends behave identically: the SQLite path and the Postgres path (D-73/D-74) return
      the same results for the same store, pinned by the existing run-twice conformance pattern.
- [ ] Redaction holds on the output — a matched session's rendered context never reveals a
      `Redactor`-registered secret, and a **secret is never usable as a search term** that could
      confirm its presence. Test covers the second case explicitly.
- [ ] The TUI session picker consumes the same filter path rather than reimplementing matching
      (coordinate with C-153, which is adding a shared fuzzy ranker to that picker).

## Progress
- (not started — filed from the 2026-07-28 Amp feature-mining pass, second pass)

## Notes
- Source: [../research/amp.md](../research/amp.md) — Amp's thread finding with keyword / file /
  repository / author / date filters. **This story exists because the first mining pass got it
  wrong**: "thread finding" was bulk-rejected along with Amp's cloud thread features, and marked
  `partial` on the assumption that `flux sessions` covered it. It does not.
- Evidence the gap is real: the `Sessions` variant in `crates/flux-cli/src/args.rs` carries only
  `--prune`; `crates/flux-events/src/store/mod.rs:149-150` and `:294-301` expose `list(limit)` and
  `list_for_account(account, limit)` and nothing else.
- Scope discipline: this is *finding a session*, not full-text search over transcripts as a product
  surface. Resist growing it into a knowledge-retrieval feature — that surface already exists
  (datasource/RAG) and is a different thing.
- Natural pairing with **C-151** (relative time in the session picker) and **C-153** (fuzzy
  filtering) — all three are the same "make sessions navigable" theme.
