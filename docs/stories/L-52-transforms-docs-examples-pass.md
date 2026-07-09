---
id: L-52
title: Data-transforms docs pass + rewrite deterministic LLM-as-mapper patterns + final gate
pillar: Language
status: done
priority:
epic: data-transforms
design: docs/designs/data-transforms.md
note: "the epic's closing story: public docs get the new vocabulary + native-text examples the user asked for; the LLM-as-mapper anti-patterns in-repo get replaced where honest"
---

# Data-transforms docs pass + rewrite LLM-as-mapper patterns + final gate

## Goal
Close the epic by making the new transform vocabulary discoverable and honest. Update
public docs (website + repo language reference) with native-text examples of every new
op, rewrite the deterministic LLM-as-mapper patterns in checked-in examples where the
rewrite is honest (not everywhere: `ai.rank` for semantic keys and `ai.extract` for
genuinely fuzzy inputs stay), and close with the full workspace gate.

## Acceptance
- [x] `website/docs/language/ops.md` — the cognition-tools table has a row for every new
      op (map / filter.where / flatten / skip / join / split / sum / count_by / group_by /
      any / all / has / pick / omit / merge_obj / coalesce / keys / values / regex_match /
      regex_extract) with a one-line semantics + a native-text example.
- [x] `website/docs/language/pure-data.md` — new "Deterministic list transforms" section
      introducing the `it`+`where` predicate convention; each Tier gets a code example.
- [x] `website/docs/language/control-flow.md` and `flows-and-syntax.md` — the L-51 native
      expr conditions get a section with `when $count > 3`, `until all({items: $checks,
      where: "it.status == 'ok'"})`, and a note on `map` vs `each` (effects/calls per
      item vs pure projection).
- [x] `examples/*.flux` rewrites where honest (verified end-to-end against the current
      binary):
  - `examples/multi-perspective.flux`: the three `$claims_* = $x.evidence` field-plucks
    → one `map({items: $scouts, path: "evidence"})` + `flatten`.
  - `examples/improve-tbench.flux`: the two "Return ONLY a JSON array of tasks" prompts
    stay (they extract genuinely fuzzy content), but any `.count`/`.status`-shaped
    predicate paths get expressed with the new ops.
  - Any example whose LLM prompt says "Return ONLY a JSON array" **and** the input is
    already structured JSON → rewrite to `map`/`filter`.
- [x] `examples/data-transforms.flux` — one new example flow that exercises every new
      op end-to-end (bounded, hermetic, no network). Auto-validated by
      `crates/flux-eval/tests/examples_validate.rs`.
- [x] Full regen sweep: `UPDATE=1 cargo test -p codewandler-flux-lang --test skill_in_sync`,
      `UPDATE=1 cargo test -p codewandler-flux-lang --test website_in_sync`, and
      `UPDATE=1 cargo test -p codewandler-flux-flow --test skill_docs_in_sync` all leave zero
      further diff.
- [x] Closing gate green: `cargo build --workspace` · `cargo test --workspace` ·
      `cargo clippy --workspace --all-targets -- -D warnings` ·
      `cargo fmt --all` (also plugins/) · `cargo test -p flux-codegate`.
- [x] Final CHANGELOG entry under `[Unreleased]` summarizing the epic with links to
      L-46..L-52.
- [x] Roadmap narrative updated to move the epic from *proposed* to *shipped*.

## Progress
- Added public docs for deterministic transforms, native expression conditions, and `map` vs `each`
  guidance across `ops.md`, `pure-data.md`, `flows-and-syntax.md`, and `control-flow.md`.
- Rewrote `examples/multi-perspective.flux` to project scout evidence with pure
  `map`/`filter`/`flatten` instead of manual field-pluck binds plus `merge`.
- Added hermetic `examples/data-transforms.flux`, validated by `crates/flux-eval/tests/examples_validate.rs`.
- Updated the roadmap to mark the data-transforms epic shipped and added the final `[Unreleased]`
  changelog summary with L-46..L-52 links.
- Closing gate is green: build, full workspace tests, clippy `-D warnings`, root/plugin fmt,
  `flux-codegate`, and all three docs drift guards.

## Notes
- Do not over-rewrite: `ai.rank` for a semantic ordering key stays (deterministic `sort`
  can't express *"security first, then logic bugs, then style"*). Same for `ai.extract`
  over prose. The rule of thumb: if the input is already structured JSON and the desired
  output is a field-shaped projection or a `field-op-value` predicate, rewrite; otherwise
  leave the LLM cell alone.
- Depends on all of L-46..L-51.
