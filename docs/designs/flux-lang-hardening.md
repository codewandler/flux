# Design: flux-lang hardening — remediate the 2026-08-01 subsystem review

**Status:** proposed · **Pillar:** Language · **Stories:** [L-113](../stories/L-113-flux-lang-hardening-epic.md) (epic) · [L-114](../stories/L-114-statement-depth-guard.md) · [L-115](../stories/L-115-each-lowering-from-cst.md) · [L-116](../stories/L-116-repeat-and-loop-budgets.md) · [L-117](../stories/L-117-confirm-carries-intents.md) · [L-118](../stories/L-118-tree-sitter-canonical-dialect.md) · [L-119](../stories/L-119-parser-fuzz-and-input-bounds.md) · [L-120](../stories/L-120-doc-drift-and-paper-cuts.md)

## Why

The 2026-08-01 adversarial subsystem review of `flux-lang`
([review](../reviews/single/2026-08-01-flux-lang-subsystem-review.md)) found that the crate's two
headline totality claims are false today — the parser SIGABRTs on ~200–900 levels of statement
nesting and `each` rejects legal strings containing `->` — plus a band of medium findings
(unbudgeted `repeat`, per-activation loop budgets, intent-less `confirm` approvals, a standing-red
tree-sitter mirror) and a set of low/info drift items. The user asked for every finding tracked
with a suggested fix. The organizing idea, taken from the review's meta-finding: each of these is a
guard whose test probed the guarded axis and missed the live one — so **every fix ships with a
failing-first test on the axis that was previously untested**.

## Approach

One story per finding cluster, ordered by severity; each story's Notes carry the suggested fix and
the exact evidence lines. The fixes for the two HIGH findings are deliberately minimal and local:

- **F1 (L-114):** thread the existing L-81 `enter()`/`leave()` guard (`parser.rs:171-183`) through
  `block`/`statement`/`block_if_indented`, and add a depth field to `lower_cst`/`cst_decode`
  traversal (or convert to an explicit worklist). Failing-first test: nested *statements* at depth
  20,000 in `deeply_nested_input_is_bounded_not_aborting` — the axis the current test omits.
- **F2 (L-115):** lower the `each` header from CST token structure (the tree already knows whether
  a top-level `ARROW` token exists) instead of `semantic_line()` text reconstruction plus
  `split_once("->")` (`cst_decode.rs:393-407`). Add `->`-bearing strings to the round-trip property
  pools so the class stays dead.
- **F3/F4 (L-116):** give the `Repeat` arm the same three protections the `loop` arm has
  (iteration budget, `cap_transcript`, `yield_now`), and decide budget scope (per-execution
  counter threaded through `exec_body`, or documented per-activation semantics) — either way the
  doc-comment at `runtime.rs:42` and the behavior must agree.
- **F5 (L-117):** build a real `IntentSet` for `confirm` from the analyzer's gathered effects/ops
  of the confirm body, so `request_approval` hands the host something checkable; alternatively
  record an explicit design decision that the label-only contract is the seam and document it in
  `host.rs`.
- **F6 (L-118):** the tree-sitter fix is upstream (`codewandler/flux-tree-sitter`) — this story
  owns it from the flux side: file/land the grammar work for bare binds, typed binds, `ctx`, `+=`,
  `goal`; move the pin; the nightly lane goes green. Coordinates with the syntax epic
  ([flux-syntax-simplification](flux-syntax-simplification.md)): grammar-surface reductions land
  upstream in the same pass.
- **F12 (L-119):** a raw-*text* fuzz/property lane (the existing property tests generate ASTs, so
  the tolerant-recovery paths are hand-case-only) plus an input-size bound before the `as u32`
  offset casts.
- **F8–F11, F13 (L-120):** batch the LOW/INFO items — `replace_ident` byte/char mix, number
  `_`-separator diagnostic, `parse` `"form"` doc omission, stale expr whitelist, `~`/HOME read on
  the pure interpolation path, feature-gate ledger count.

Out of scope for this epic: F7 (spec rewrite) and the `fluxlang fmt` subcommand — owned by the
syntax-simplification epic (L-102); F8's full typing milestone (tracked in the evolution plan);
bus factor (structural).

## Alternatives considered

- **One mega-story "fix the review".** Rejected: the findings differ in risk, size, and blast
  radius; several are release-blocker-adjacent while others are doc sweeps.
- **Folding hardening into the syntax epic.** Rejected: different theme (safety/totality vs
  authored-surface design); mixing them would blur both boards.

## Risks & open questions

- Whether any production path reaches `execute_flow` without `lower()` decides F3's real-world
  severity — needs a flux-flow read (tracked in L-116's acceptance).
- WASM stack limits may need a lower `MAX_PARSE_DEPTH` for the portable build (L-114).
- Intent-bearing `confirm` (L-117) touches the `OpHost` trait — a seam the engine adapts; check
  protocol-line/version implications before changing the signature.

## Acceptance / done

Union of L-114…L-120 acceptance. The epic closes when the review's triage block flips to
`handled` with these stories recorded as owners.
