---
id: L-12
title: Strict review — typed artifacts + deterministic aggregator (Phase 3)
pillar: Language
status: done
epic: strict-review-flows
design: docs/designs/strict-review-flows.md
note: ReviewRequest/ReviewFinding/ReviewReport + review.normalize/aggregate (fingerprint/dedupe/rank)
---

# Strict review — typed artifacts + deterministic aggregator (Phase 3)

## Goal

Give the protocol typed, reusable artifacts and a deterministic aggregator: `ReviewRequest`,
`ReviewFinding`, and `ReviewReport` (as schemas first, promoted to prelude types once multiple
surfaces consume them) plus `review.normalize` / `review.aggregate` composite ops that parse reviewer
output, quarantine malformed findings as gaps, fingerprint by category/file/line/normalized-title,
deduplicate, and rank by severity/confidence/agreement with stable ordering. Serves the Language
pillar: aggregation is deterministic runtime, and the model is used only for final prose synthesis
against a fixed schema — never to decide which tools to run or reviewers to spawn.

Full design: [docs/designs/strict-review-flows.md](../designs/strict-review-flows.md) — Phase 3 &
"Aggregation".

## Acceptance

- [x] **Failing-first test:** given fixed reviewer outputs, `review.aggregate` produces a report with
  **stable ordering** across runs, and a malformed reviewer output is reported as a **gap** (not
  silently accepted) — added red, then green.
- [x] `ReviewRequest`/`ReviewFinding`/`ReviewReport` exist as schemas (embedded first; prelude-type
  promotion tracked when a second surface consumes them).
- [x] `review.normalize`/`review.aggregate` implemented as deterministic composite ops (native Rust
  only if fingerprinting/ranking needs a stable built-in).
- [x] Duplicate findings collapse by fingerprint; ranking is by severity, then confidence, then
  reviewer agreement.
- [x] `strict_review` (L-10) is migrated to emit a typed `ReviewReport`.
- [x] Dev loop green: `cargo build/test --workspace`, `clippy -D warnings`, `fmt`, `flux-codegate`.
- [x] CHANGELOG entry.

## Notes
- Open question settled: reviewer disagreement is **merged with an agreement count** (`agreement`
  field on `ReviewFinding` — the number of distinct reviewers that raised the same fingerprint), not
  preserved as separate findings.
- Depends on [L-10](L-10-strict-review-example-flow.md); consumed by
  [L-13](L-13-strict-review-journey-cli.md).

## Progress
- Implemented `review.normalize`/`review.aggregate` as **native Rust ops** (not flow-level composite
  ops) in `crates/flux-tools/src/cognition.rs`, registered in `register_cognition` and the `cognition`
  tool group (force-on, matching `dedupe`/`sort`/etc.). Fingerprinting needs a stable, deterministic
  hash unaffected by `HashMap` iteration order — a native built-in, not a flow-level composite of
  existing ops — so the "native Rust only if fingerprinting/ranking needs a stable built-in" escape
  hatch was exercised.
  - `review.normalize({ findings })` → `{ findings, gaps }`: parses each raw reviewer entry, computing
    a stable fingerprint (`category + file + line + normalize(title)`, hashed via a fixed-key
    `DefaultHasher`) for well-formed entries and quarantining malformed ones (not an object; missing
    `title`/`category`; missing or invalid `severity`) into human-readable `gaps` strings
    (`"dropped malformed finding: …"`) — never silently dropped, never surfaced as findings.
  - `review.aggregate({ findings, files, reviewers })` → a full `ReviewReport`: runs the same
    normalize step, dedupes by fingerprint (via a `BTreeMap`, counting distinct non-empty `reviewer`s
    as `agreement`, taking the max `confidence` across the group), then ranks severity desc → confidence
    desc → agreement desc → fingerprint asc (stable tiebreak, so ordering is byte-identical across
    runs regardless of input order).
  - `ReviewFinding`/`ReviewReport` are `schemars`-derived Rust structs in `flux-tools` (embedded schema
    via `flux_spec::tool_input_schema`), per the story's explicit "do NOT add to
    `flux_lang::prelude`/`PRELUDE_TYPES`" instruction — promotion deferred. No separate `ReviewRequest`
    struct was added: reviewer input is just the role's task prompt string (built via `fmt` in the
    flow), so there was no aggregator-facing shape for it to schema; this is noted as a deliberate
    minimal-scope call per the story's "keep minimal" allowance.
- Migrated `examples/strict_review.flux`: the aggregation tail (`merge` → `filter` → `dedupe` → `sort`
  → hand-built summary) is now `return review.aggregate({ findings: $all_findings, files: $files,
  reviewers: [...] })` — one deterministic native call. The read-only gather + `parallel` fan-out are
  unchanged.
- Updated the three reviewer roles (`.flux/agents/review-{security,correctness,maintainability}.md`)
  to drop `fingerprint`/`rank` from their JSON output contract — the aggregator now computes both;
  reviewers still emit `severity`/`category`/`file`/`line`/`title`/`evidence`/`recommendation`/
  `confidence`/`reviewer`.
- Updated the L-10 integration test `crates/flux-sdk/tests/strict_review.rs`: mock reviewers no longer
  emit `fingerprint`/`rank`; a "shared finding" raised by both the security and correctness mock
  reviewer now asserts `agreement == 2` after dedup; the maintainability mock's malformed bare-string
  entry now asserts a `gaps` entry (not a silently-dropped finding); ranking asserted as
  severity → confidence → agreement across 4 distinct-severity findings. Both tests (structured-report
  shape + stable-ordering-across-runs) stay green.
- Added 3 new unit tests in `crates/flux-tools/src/cognition.rs` (added red — they referenced
  `ReviewAggregateTool`/`ReviewNormalizeTool` before those types existed — then green): stable
  ordering + malformed→gap, fingerprint stability/distinctness + duplicate-collapse-with-agreement,
  and severity→confidence→agreement ranking order. Also extended `registers_all_named_ops` and
  `flux-tools`'s top-level `builtins_register` catalog test, and the `cognition` `ToolGroup` manifest
  in `groups.rs`, with the two new op names.
- Full gate green: `cargo build --workspace`, `cargo test --workspace` (all crates, incl.
  `flux-sdk`'s `strict_review.rs`), `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo fmt --all --check`, `cargo test -p flux-codegate` (`flux-tools` stays L2 — no new crate
  dependency was needed for these ops).
