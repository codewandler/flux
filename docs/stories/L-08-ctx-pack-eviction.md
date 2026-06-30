---
id: L-08
title: Fix ctx-pack eviction — drop-overflow-and-continue + keep priority
pillar: Language
status: done
priority:
epic: session-s251-postmortem
design: docs/designs/session-s251-postmortem.md
note: a single oversized early member drops every smaller member after it (hard `break` in build_ctx); the s_251 reasoning spiral
---

# Fix ctx-pack eviction — drop-overflow-and-continue + keep priority

## Goal

Stop the `ctx`/`ctx_append` packer from evicting the working set. Today a single oversized member
declared early triggers a hard `break` that drops every smaller-but-valuable member after it, even
ones that would individually fit — which is what starved `ai.reason` and spiralled session `s_251`
turn 2 into a 7-iteration cancelled loop. Serve the Language pillar value: context packs must
preserve the highest-information members within budget, not a declared-order prefix.

## Acceptance

- [x] **Failing-first test** — `ctx_pack_keeps_small_members_after_an_oversized_one` in
      `crates/flux-lang/src/runtime.rs` (fails on the old hard-`break` packer, passes after the fix).
- [x] **Drop-and-continue** — a member that doesn't fit is skipped and packing continues; it lands in
      `dropped` on the `CtxShrunk` event.
- [x] **Keep priority** — visibility-tier priority retained (`Pinned` > `Visible`); the existing
      `ctx_budgets_by_visibility_and_appends_immutably` + `ctx_append_re_budgets_and_can_evict` tests
      still pass (no rank inversion).
- [x] `CtxShrunk` `kept`/`dropped` accurate; existing shrink tests green.
- [x] Root gate green: `cargo test -p flux-lang`, `clippy -D warnings`, `fmt`, `skill_in_sync`
      (SKILL.md regenerated from the `skill.rs` prose), `skill_docs_in_sync`, plus the full workspace
      gate (`cargo test --workspace`, `cargo test -p flux-codegate`) — all green.
- [x] CHANGELOG entry under `[Unreleased]`.

## Progress

- **Audited the post-mortem files for sensitive data** — redacted the one leaked AWS account ID /
  EKS ARN in `docs/designs/session-s251-postmortem.md` to a neutral placeholder; re-scanned all created
  docs (design, two stories, roadmap, board) for AWS account IDs / EKS ARNs / endpoint hostnames:
  clean.
- **Cache-impact analysis** — confirmed `build_ctx` runs inside the value store and its `Ctx` output
  is a per-iteration tool result, never the cached system prompt. Prompt caching
  (`crates/flux-providers/src/messages/mod.rs:114`, `system_field`, ≥4096-char system prompt only) is
  cache-neutral to this change: the cache key (system prompt text) is byte-identical before/after.
- **Failing-first test** — wrote `ctx_pack_keeps_small_members_after_an_oversized_one`; confirmed it
  FAILS on the current `break` packer (panics on "small member after the oversized one is kept").
- **Fix** — `build_ctx` now `continue`s past an oversized member instead of `break`-ing
  (`crates/flux-lang/src/runtime.rs`); updated the `build_ctx` doc-comment.
- **Docs** — added the drop-and-continue note to `reference.md` §ctx (hand-written prose, outside
  the generated markers) and to `skill.rs::render()` (the `SKILL.md` source); regenerated `SKILL.md`
  via `UPDATE=1 cargo test -p flux-lang --test skill_in_sync`. Left the auto-generated `node-kinds` /
  `prelude-types` tables untouched (still in sync with `ast.rs`).
- **Gate** — `cargo test -p flux-lang` (incl. `skill_in_sync`), `cargo test -p flux-flow --test
  skill_docs_in_sync`, `cargo build --workspace`, `cargo test --workspace` (0 failures),
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all --check`,
  `cargo test -p flux-codegate` — all green.

## Notes

- Root cause + evidence in [docs/designs/session-s251-postmortem.md](../designs/session-s251-postmortem.md)
  §Defect 1. The oversized member in `s_251` was `analysis_session_evidence_fresh` at 492,648 chars
  (a full session evidence dump); it preempted 11k/24k/12k code reads that would have fit.
- Implementation site: `crates/flux-lang/src/runtime.rs` `build_ctx` (~line 3275), the `else { break; }`
  branch and the `order.sort_by_key` priority. `vis_keep_rank` (~line 3260) is the visibility ladder.
- Op-agnostic: consuming ops (`ai.reason`, etc.) read the already-bounded member list — no op-semantics
  change. The interpreter stays agnostic.
- Sibling story [D-33](D-33-endpoint-discovery-aliases.md) fixes the discovery-side defect that
  compounded with this one; either fix alone helps, both are needed for the "check db connectivity"
  path to be trustworthy.
