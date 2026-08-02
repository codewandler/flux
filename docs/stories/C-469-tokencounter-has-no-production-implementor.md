---
id: C-469
title: "`TokenCounter` has no production implementor — every token budget runs on a 4-chars estimate"
pillar: Core
status: ready
priority: 6
areas: [flux-provider, flux-lang]
note: "the trait exists, one test FixedCounter implements it, and the single production call site passes None. C-462's re-scaling decision needs real token counts, and this is why it does not have them"
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
- [ ] **Or retire it**: remove the trait and the `Option` parameter, and state at `estimate_tokens` that
      flux budgets on characters deliberately, with the reasoning. A removed extension point is honest;
      an unmet one is not.
- [ ] Either way, `estimate_tokens`'s doc comment says whether an exact path exists in practice, so the
      next reader does not have to grep for implementors to find out.

## Notes

- ⚠ `TokenCounter` is public API on `codewandler-flux-provider`. Removing it is a **breaking change** to
  a published crate and carries a version decision under the repo's pre-1.0 rule (minor = breaking).
  Check for external implementors before choosing the retire branch — "no implementor in this tree" is
  not "no implementor anywhere".
- **This is the load-bearing dependency of [C-462](C-462-compaction-threshold-is-context-window-blind.md).**
  C-462 asks whether the compaction threshold should scale with a model's context window and notes it
  needs evidence it does not have; it also asks *"Does flux know a model's context window at all?"* The
  answer to the adjacent question is now known: flux does not know a request's token count either. If
  C-462 goes the scaling route, it needs this first.
- Verified 2026-08-02 against `main`: the only `impl TokenCounter` is the `#[cfg(test)]` `FixedCounter`,
  and the only `slice_context` caller passes `None`.
- Filed 2026-08-02 out of C-441's review.
