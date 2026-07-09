---
id: L-50
title: Regex ops — `regex_match`, `regex_extract` (ReDoS-free via Rust `regex`)
pillar: Language
status: done
priority:
epic: data-transforms
design: docs/designs/data-transforms.md
note: "regex was only accessible inside the `grep` file tool; ship two pure ops so plans can classify or extract from any string result"
---

# Regex ops — `regex_match`, `regex_extract`

## Goal
Bring deterministic regex matching / extraction into the pure-op catalog. Rust's `regex`
crate is linear-time by construction (Thompson NFA, no backtracking), so this is
**ReDoS-free by design** — the safest way to expose regex to plans. Two ops:
`regex_match` for boolean predicates (drop-in for `when`), and `regex_extract` for
projection.

## Acceptance
- [x] `regex_match({s, pattern})` — returns `"true"`/`"false"` (matches the boolean-emitter
      convention). Pattern compiled via `RegexBuilder::size_limit(1 MiB)`; pattern length
      `> 512` chars → clear error before compilation. Invalid pattern → clear error naming
      the compile failure. Failing-first tests: `regex_match_true_false`,
      `regex_match_rejects_oversize_pattern`, `regex_match_reports_bad_pattern`.
- [x] `regex_extract({s, pattern, group?, all?})` — `group` defaults to `0` (whole
      match); with `all: true`, returns a JSON array of every match (of the requested
      group). Without `all`, returns the first match as a string, or `null` if no match.
      Missing capture group index → clear error. Failing-first tests:
      `regex_extract_first_and_all`, `regex_extract_null_on_no_match`,
      `regex_extract_bad_group_errors`.
- [x] Both ops registered in `register_cognition` and the `cognition` group; group
      description updated.
- [x] `website/docs/language/ops.md` cognition-tools table gains rows for both, with
      native-text examples (SemVer extraction, "does log line contain ERROR").
- [x] CHANGELOG entry under `[Unreleased]`.

## Progress
- Implementation and acceptance tests are present in `flux-tools` cognition ops, using Rust
  `regex` with pattern length and compiled-size limits.
- Website docs already had the native-text examples; this pass also added the ops to the engine
  ops reference. The op-addition changelog entry shipped with `v0.10.0`; `[Unreleased]` records the
  reference-doc completion so the story trail stays current without pretending the ops are new.

## Notes
- No dependency on L-46 — safe to work in parallel with L-49 while L-46 is in flight.
- Deliberate scope-out: `regex_replace` (the expr `replace` builtin covers literal
  replacements; extending it to regex can be a later story on evidence).
- The `regex` crate is already a workspace dep (`grep` file tool uses it — see
  `flux-tools/src/lib.rs:1388`); no new dependency.
