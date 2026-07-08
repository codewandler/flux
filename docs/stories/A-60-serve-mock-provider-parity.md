---
id: A-60
title: Program `--serve -m mock` provider parity (behave like the CLI mock path, or reject clearly)
pillar: Agent
status: done
epic: beta-hardening
design: docs/designs/beta-hardening.md
note: "F-014 (beta rec #4): `app run <program> --serve … -m mock` exposes the card but message/send takes the Anthropic path and fails on low credits; `-m codex/gpt-5.5` works on the same served agent — the served path ignores the mock provider selection"
---

# Program `--serve -m mock` provider parity

## Goal
`flux app run <program> --serve … -m mock` exposes the program's AgentCard, but an inbound
`message/send` runs the **Anthropic** path (and fails when credits are low) instead of the mock
provider — the same served agent works with `-m codex/gpt-5.5`. The served program path is not
honoring the `-m mock` selection the way the normal CLI path does. Either wire the served path to
the same mock provider behavior, or reject `--serve -m mock` with a clear, actionable error.

## Why (evidence)
- Beta F-014: "`flux app run <program> --serve … -m mock` exposed the program AgentCard, but
  `message/send` used the Anthropic path and failed for low credits. With `-m codex/gpt-5.5`, the
  same served agent worked."

## Acceptance
- [ ] Preferred: a served program under `--serve -m mock` answers `message/send` via the same mock
      provider the non-served CLI mock path uses (no Anthropic/network call).
- [ ] Acceptable fallback (if mock-over-serve is intentionally unsupported): `--serve` with `-m mock`
      is rejected at startup with a clear message naming the unsupported combination and the
      supported providers — never a silent fall-through to Anthropic.
- [ ] Failing-first test: a served program with `-m mock` either returns the mock response over the
      A2A wire, or fails fast at startup — but does **not** attempt the Anthropic path.
- [ ] Docs (`--serve` + mock-mode guidance) state which providers `--serve` supports.

## Progress
- 2026-07-08 **DONE.** Chose the preferred path (honor mock, not reject). Extracted
  `app_provider_for(spec)` in flux-cli and gave it the same mock guard the other entry points
  (`build_agent`/`provider_for`/REPL) use, so `app run <program> --serve -m mock` resolves the
  offline `MockCliProvider` instead of falling into `build_provider`'s Anthropic short-alias arm
  (which took the Anthropic path and failed on low credits). Test: `app_serve_provider_honors_mock`
  (asserts the resolved provider's `name()` is `mock`, not `anthropic`).

## Notes
- Beta rec order #4.
- Trace how the served program builds its provider vs. how `flux run`/`flow run` resolve `-m mock`;
  the divergence is the served agent's provider construction.
- Related docs item: F-001 (mock-mode overpromise) is tracked in [C-46](C-46-beta-docs-truth-pass.md).
- Epic: [beta-hardening](../designs/beta-hardening.md).
