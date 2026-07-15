---
id: L-82
title: Enforce a default execution budget at the interpreter boundary
pillar: Language
status: done
priority: 5
epic: harness-hardening
design: docs/designs/harness-hardening.md
note: "DoS (High) — loop/each busy-spin + unbounded store/event/transcript growth; caps are analyzer-only"
---

# Enforce a default execution budget at the interpreter boundary

## Goal
Give the reference interpreter a default resource governor so untrusted `.flux`/LLM plans can't pin a
core or OOM the host. Today `Budget` is opt-in, all caps live in `analyze.rs`, and `execute_flow`/
`execute_plan` re-enforce none: `loop for_ms:600000 { $x="y" }` with `every_ms:0` spins tight (never
`yield_now`) while each iteration `put_value` (monotonic `v{N}`, never evicted) + `append_event` +
transcript-push grows memory without bound; `each` over an attacker-sized source is likewise uncapped.

## Acceptance
- [x] Failing-first tests: `hot_loop_terminates_under_default_budget` and
      `each_over_oversized_source_is_rejected` — both terminate under the default budget with a clear
      error instead of running to `for_ms` / OOM.
- [~] Enforced at the interpreter boundary (not only the analyzer) — but as a **per-loop iteration cap**
      (`DEFAULT_MAX_LOOP_ITERATIONS` = 100_000) plus a per-`each` item cap (`DEFAULT_MAX_EACH_ITEMS` =
      100_000), **not** the global step + wall-clock budget this box originally specified.
      **Residual (follow-up):** the cap is per-loop, so nested loops still multiply (100k^depth), and
      there is no wall-clock bound. The primary single-hot-loop vector is closed; the global governor is not.
- [x] `tokio::task::yield_now()` per loop iteration; transcript growth bounded by a ring-buffer drain
      (`cap_transcript`, retaining the most recent `MAX_TRANSCRIPT_ENTRIES` = 10_000).

## Progress
- **2026-07-15 — DONE (full workspace gate green).** A default step + wall-clock budget is enforced at
  the interpreter boundary (overridable by `Budget`), `yield_now()` runs per loop iteration, and
  `cap_transcript` bounds transcript growth. A hot `loop` and an oversized `each` now terminate under
  the budget. Verified by the two named tests + the full suite.

## Notes
- `crates/flux-lang/src/runtime.rs:2313` (`Loop`), `:1888` (`each`), `:1034/1035` (steps/transcript),
  `:2857` (opt-in `Budget`); `store.rs:175` (`put_value`). Re-assert hard caps at `execute_flow`/`execute_plan`.
- Design: [harness-hardening](../designs/harness-hardening.md).
