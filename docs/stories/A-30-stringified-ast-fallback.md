---
id: A-30
title: "Tolerant stringified-ast decode — accept emit_plan's ast as a JSON string, not just an object"
pillar: Agent
status: done
epic: parse-resilience
design: docs/designs/parse-resilience.md
note: "s_360/s_361: qwen3.7-max (and -plus) double-encode `ast` — a JSON string containing a VALID plan — and re-emit the same shape through all 8 repair steps; one from_str fallback makes the turn succeed"
---

# Tolerant stringified-ast decode in emit_plan

## Goal
Models trained on the OpenAI wire habitually double-encode nested tool arguments: `emit_plan`
arrives as `{"ast": "<JSON-encoded string>"}` instead of `{"ast": {…}}`. The `EmissionArm::Json`
decode in `crates/flux-flow/src/compile.rs` rejects the string outright and the repair loop never
converges (qwen3.7-max re-sent the identical shape 8/8 steps in s_360/s_361 despite the serde error
echoed back). When the `ast` value — or the whole-`input` fallback — is a JSON string, parse the
string as JSON first and decode `DraftAst` from the result; keep the original strict error when
that inner parse fails. Encoding tolerance only: the decoded plan traverses the unchanged
hidden-op, gather, and `validate_plan` gates.

## Acceptance
- [x] Failing-first test: `emit_plan` with `ast` as a **string-encoded valid plan** compiles to the
      same accepted plan as the object form (mock provider fixture shaped like qwen's s_361
      traffic). Fails today with "invalid type: string … expected struct DraftAst".
      → `json_arm_accepts_a_string_encoded_ast` (failed with exactly that error, then green).
- [x] A string that is not valid JSON (or parses to JSON that is not a `DraftAst`) still produces
      the informative "invalid AST JSON" repair feedback — tolerance never silently accepts garbage.
      → `json_arm_garbage_string_ast_surfaces_the_decode_error`.
- [x] Downstream gates are provably unchanged: a string-encoded plan containing a hidden op is
      rejected exactly like its object twin (test).
      → `json_arm_string_encoded_hidden_op_is_still_rejected`.
- [x] The text arm (`EmissionArm::Text`) is untouched (all existing text-arm tests unchanged+green).

## Progress
- 2026-07-03 filed from the s_360 diagnosis (live repro s_361 with temp instrumentation).
- 2026-07-03 **done**: string-unwrap in the `EmissionArm::Json` decode (`compile.rs`), 3
  failing-first tests, full gate green. **Live-verified**: `flux plan -m
  openrouter/qwen/qwen3.7-max "review security model of this lib"` — the exact s_360 model+prompt —
  now compiles a clean 15-branch parallel gather plan on the FIRST emission.

## Notes
- Decode site: `crates/flux-flow/src/compile.rs`, `EmissionArm::Json` arm of the `emit_plan`
  handler (~line 618).
- Sibling model quirk: GLM 5.2 (2026-06-30) emitted structurally malformed JSON — that one is NOT
  fixable by unstringifying; this story only covers the well-formed-but-string-encoded class.
- Epic: [parse-resilience](../designs/parse-resilience.md).
