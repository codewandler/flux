---
id: C-469
title: "`TokenCounter` has no production implementor — every token budget runs on a 4-chars estimate"
pillar: Core
status: done
priority: 6
areas: [flux-provider, flux-lang]
note: "retired both unrelated, unused TokenCounter seams; context slicing now documents its deliberate deterministic 4-chars/token policy, with the breaking API removal queued for the next minor"
---

# An optional capability nobody provides

## Goal

Either give `TokenCounter` a real implementor and wire it to the one call site that asks for it, or
record that flux deliberately budgets on an estimate and remove the unmet extension point.

## The finding

`crates/flux-provider/src/lib.rs:208-212` declares the capability:

```rust
/// Optional capability: count the prompt tokens of a request before sending it.
#[async_trait]
pub trait TokenCounter: Send + Sync {
    async fn count_tokens(&self, req: &Request) -> Result<u64>;
}
```

Nothing in the tree implements it except a test fixture — `FixedCounter` at
`crates/flux-lang/src/context_slice.rs:833`, inside a `#[cfg(test)]` module. No provider implements it:
not Anthropic, not OpenAI, none of them, though every one of those APIs exposes a token-count endpoint
or a tokenizer.

The one production call site passes `None` (`crates/flux-lang/src/runtime.rs:4643-4644`), so
`size_of` (`context_slice.rs:360`) always falls through to `estimate_tokens` — the documented
*"deterministic fallback estimate … ~4 [chars per token]"* (`:353`). Context slicing — which decides
what the model is allowed to see — is therefore budgeted entirely on a heuristic, and the exact path is
dead code reachable only from tests.

⚠ **The fallback is not wrong; the silence is.** A 4-chars-per-token estimate is a defensible default.
What is not defensible is a trait documented as *"optional capability"* that is optional in the sense
that it has never once been exercised — a reader (and this repo has a documented history of exactly this
defect class) reads the `Option<&dyn TokenCounter>` parameter and concludes flux counts tokens exactly
when a provider can. It does not, anywhere.

## Acceptance

Pick one and do it properly; both are legitimate.

- [ ] **Implement it**: at least one provider counts tokens for real, `runtime.rs:4643` passes it, and a
      failing-first test pins that a request whose exact count differs materially from the estimate is
      sliced according to the *exact* count. ⚠ It must also pin the fallback: when the counter errors or
      is absent, slicing still succeeds on the estimate and never fails closed on a budget question.
- [x] **Or retire it**: remove the trait and the `Option` parameter, and state at `estimate_tokens` that
      flux budgets on characters deliberately, with the reasoning. A removed extension point is honest;
      an unmet one is not.
- [x] Either way, `estimate_tokens`'s doc comment says whether an exact path exists in practice, so the
      next reader does not have to grep for implementors to find out.

## Decision

Retire both extension points and keep deterministic estimated budgeting. The similarly named traits
were not one capability: `flux-provider::TokenCounter` asynchronously counted a complete provider
`Request`, while `flux-lang::context_slice::TokenCounter` synchronously counted one text candidate.
Connecting them directly would invert the L0 → L1 dependency boundary, and there was no production
owner that adapted one into the other. The sole runtime `slice_context` call also uses that function
for visibility gating with no budget and empty candidate text; it never asked for token sizing at all.

The public `estimate_tokens` documentation now owns the real policy and its rationale. The provider
trait, Flux-Lang trait, and sixth `slice_context` argument are removed. This is source-breaking for
direct callers of either published crate, so it rides the next pre-1.0 minor release. Full reasoning
is recorded in [deterministic token budgeting](../designs/deterministic-token-budgeting.md).

## Progress

- 2026-08-03: added a failing-first compile check by changing the estimator test to the intended
  five-argument `slice_context` API; it failed on the still-required counter parameter.
- 2026-08-03: audited the public ecosystem before removing the provider trait. crates.io reported
  ten reverse dependencies, all Flux workspace crates from this repository; GitHub code search found
  `flux_provider` plus `TokenCounter` only in `codewandler/flux`.
- 2026-08-03: removed both unused traits and all counter plumbing, pinned deterministic estimator
  behavior, and documented the breaking API decision.
- 2026-08-03: focused provider and Flux-Lang checks, the Flux-Lang CLI feature checks, and the full
  workspace build/test/clippy/fmt/codegate gate all pass.

## Notes

- ✅ `TokenCounter` was public API on `codewandler-flux-provider`. Its removal is a **breaking change** to
  a published crate and carries a version decision under the repo's pre-1.0 rule (minor = breaking).
  The compatibility audit checked external implementors before choosing retirement — "no implementor
  in this tree" was not treated as "no implementor anywhere".
- **This was the load-bearing dependency of [C-462](C-462-compaction-threshold-is-context-window-blind.md).**
  C-462 asks whether the compaction threshold should scale with a model's context window and notes it
  needs evidence it does not have; it also asks *"Does flux know a model's context window at all?"* The
  answer to the adjacent question is now known: Flux deliberately does not preflight a request's exact
  token count. C-462 subsequently kept the compaction trigger as a fixed character budget.
- Verified 2026-08-02 against `main`: the only local `impl TokenCounter` was the `#[cfg(test)]`
  `FixedCounter`, and the only `slice_context` caller passed `None`. Implementation investigation
  refined this finding: the provider and Flux-Lang traits were unrelated, and the runtime call used
  no budget, so its `None` never selected an estimator-backed production budget.
- Filed 2026-08-02 out of C-441's review.
