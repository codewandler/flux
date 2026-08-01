# Design: flux-lang hardening — remediate the 2026-08-01 subsystem review

**Status:** proposed · **Pillar:** Language · **Stories:** [L-113](../stories/L-113-flux-lang-hardening-epic.md) (epic) · [L-114](../stories/L-114-statement-depth-guard.md) · [L-115](../stories/L-115-each-lowering-from-cst.md) · [L-116](../stories/L-116-repeat-and-loop-budgets.md) · [L-117](../stories/L-117-confirm-carries-intents.md) · [L-118](../stories/L-118-tree-sitter-canonical-dialect.md) · [L-119](../stories/L-119-parser-fuzz-and-input-bounds.md) · [L-120](../stories/L-120-doc-drift-and-paper-cuts.md) · [L-123](../stories/L-123-flows-that-execute-with-no-analyzer-gate.md)

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

## The static-analysis invariant (L-123)

L-116's census asked which production paths reach `execute_flow` without `lower()`. The answer was
"several", and they disagreed with each other for no stated reason. L-123 settles the rule, so the
next entry point added knows which side of it it is on:

> **A flow body this engine did not itself produce is `analyze_flow`-gated before it executes.**
> A body that *is* engine output — replayed, resumed, or sliced from a plan already accepted and
> executed once — is exempt, and says so at its call site together with what backstops it.

The line is "fresh input vs. engine output", not "trusted vs. untrusted". None of these paths is
model- or remote-reachable: the agent loop, `flow_run`, HTTP/A2A/MCP were already clean, and the
rest are local-operator inputs — someone who can run `flux session fork --edit` can already run
arbitrary commands. **This is consistency and defence-in-depth, not a closed vulnerability.**
Analysis is a *static contract* check (op resolution, arity, symbol definedness, structural
legality); it is never the authorization boundary. Every op still traverses `Executor::dispatch`,
and L-116's per-execution loop budget still bounds iteration at run time.

**Gated — fresh input:**

| Entry point | Where |
| --- | --- |
| the agent loop's AST, at assembly | `flux-flow/src/engine.rs` (`validate_agent_loop`) |
| the model's `flow_run` JSON AST | `flux-flow/src/loop_host.rs`, via `analyze::lower` |
| authored `flux flow run` | `flux-cli/src/flow_cmd.rs`, via `analyze::lower` |
| `flux session fork --edit <file>` | `flux-flow/src/fork.rs` (`analyze_edited`) — L-123 |
| `flux app` journeys | `flux-app/src/app.rs` (`analyze_journey`) — L-123, minus definedness (below) |

**One documented carve-out: symbol definedness for journeys.** A journey's symbol environment is
*payload-shaped* — `seed_payload` binds `$input` plus one symbol per top-level field of whichever
event arrived — so "is `$delivery` bound?" is a fact about a delivery, not about the program, and a
journey may legitimately read a field only some events carry. Since `analyze_flow`'s definedness
rule exists to have **zero false positives** (L-15/F5), honouring that in a dynamic environment
means treating every referenced symbol as potentially payload-supplied. `analyze_journey` therefore
prebinds the session's real symbols *union everything the body reads*. Everything statically
decidable stays on: op resolution, arity, declared-name validity, expression-position legality,
loop bounds, `parallel` disjointness, `await`/`checkpoint` placement. An unbound `$var` stays a
precise runtime error at the statement that reads it. The fork and CLI doors, whose stores are
fully populated before the flow runs, keep the definedness check.

**Exempt — engine output, each with its reason at the call site:**

| Entry point | Why, and what backstops it |
| --- | --- |
| fork prefix replay, `diverge_inject` | a slice of an already-executed recorded plan; a slice is not a standalone flow, so analysis would reject valid forks over symbols bound outside it. Cassette-served or envelope-dispatched; loop budget at run time. |
| journey ask-resume (`resume_flow`) | the suspension latch's own copy of a body `run_journey` already gated, resumed mid-flow. Named gap: a suspension persisted by a pre-gate build resumes ungated. |

**Deliberately opt-in, documented at the public surface:** four `FlowClient` doors — `execute`,
`execute_with`, `execute_with_sink` and `execute_streamed` — do **not** call `analyze()` for you.
Forcing it would break `execute_with`'s seeding (seeded `$name`s are unbound to plain `analyze`;
only `analyze_seeded` sees them) and would re-pay the cost on every run of a stored,
already-validated AST. The embedder owns the check on those four, and each carries a pointer saying
so.

⚠ **This is per-door, not a property of `execute*` as a family** — an earlier draft of this
paragraph said otherwise and was wrong. Two doors *do* analyze: `FlowClient::run_flow` is
`parse → analyze → execute_with` by construction, and `FlowClient::execute_optimized` calls
`optimize` → `flux_flow::analyze::lower`, whose first statement is `analyze_flow` — strictly more
than `analyze`. A blanket claim here would put two guaranteeing doors on the ungated side of the
very table this section exists to be, so state the door, never the family.

One consequence worth recording: gating journeys made the **deprecated 2+-positional call form**
(`send("cli", $reply)`) a startup error there, as it already was under `flux flow run`.
`map_args_to_input` still accepts it at run time, deliberately and only so a *legacy stored plan*
does not fail mid-flight after side effects — that fallback was never a licence for new authored
source.

**Still open, and judged out of proportion for L-123:** L-116's remaining fresh-counter boundary is
a composite op call, which re-enters `execute_flow` with a fresh loop budget, so
`loop { call composite_that_loops() }` still multiplies — bounded by `DEFAULT_MAX_COMPOSITE_DEPTH`
(8), not by the budget. Closing it means threading a budget handle through
`run_call`/`eval_cond`/`execute_composite_call`: a change to the interpreter's hot call path, in a
different subsystem from this story's call-site gates, with its own failing-first test burden. It
wants its own story rather than a rider on this one.

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
