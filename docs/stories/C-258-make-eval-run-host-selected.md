---
id: C-258
title: "Make eval execution host-selected, sandbox-honest, and credential-minimal"
pillar: Core
status: done
priority: 3
epic: adversarial-review-remediation-2026-07-30
design: docs/designs/adversarial-review-remediation-2026-07-30.md
areas: [flux-eval, flux-system, flux-cli]
note: "HIGH — eval_run accepts model-controlled flux_bin, exempts it from sandboxing, and injects raw provider keys"
---

# Make eval execution host-selected, sandbox-honest, and credential-minimal

## Goal

Preserve benchmark execution while making it impossible for model input to choose a sandbox-exempt
program that inherits provider credentials.

## Acceptance

- [x] Failing-first tests prove a caller-supplied `flux_bin` can select an arbitrary executable and
      receive a sentinel provider key today, then prove both are impossible after the change.
- [x] The executable under test is selected by trusted host configuration or an opaque prevalidated
      identifier; `EvalRunInput` cannot carry an arbitrary path into `argv[0]`.
- [x] Model-reachable eval processes use the ordinary sandboxed process path. Any remaining exemption
      is unreachable from tool input and documents its trusted selector.
- [x] Child environments contain only credentials explicitly required by the selected provider and
      never copy `FLUX_SECRET` or unrelated provider keys wholesale.
- [x] The production catalog and direct-I/O enforcement classify `flux-eval` honestly as a
      model-facing operation pack.
- [x] Eval/CLI/system tests and the standard gate are green.

## Progress

- 2026-07-30 — failing-first regressions removed `flux_bin` from the model schema and rejected the
  legacy field at execution; `FLUX_EVAL_BINARY` is now the sole trusted-host selector, with the
  running executable as its default.
- 2026-07-30 — local and terminal-bench children use the ordinary sandbox-aware `System` path. The
  local runner has no exemption call, and terminal-bench resolves the host sandbox posture instead
  of silently constructing an off-sandbox driver.
- 2026-07-30 — credential forwarding is selected from the task's provider: Anthropic, OpenAI,
  OpenRouter, and AWS receive only their own required variables; OAuth/local/mock providers receive
  none. `FLUX_SECRET`, unrelated provider keys, and fixture attempts to reintroduce them are refused
  by tests.
- 2026-07-30 — `cargo test -p flux-eval`, the structural direct-I/O guard, targeted tools/spec
  tests, the public-environment documentation contract, and scoped clippy with `-D warnings` are
  green. The website configuration reference documents the trusted selector.
- 2026-07-30 — independent follow-up review found equivalent terminal-bench selectors (`tb_bin`,
  `flux_binary`, import paths, and `rebuild`) still reachable through open adapter parameters. The
  story was returned to `in-progress` pending host-only selection and a regression covering the
  credentialed child process.
- 2026-07-30 — terminal-bench now rejects legacy `flux_bin`, `flux_binary`, `tb_bin`,
  `agent_import_path`, `pythonpath`, `dataset`, and `rebuild` inputs. The flux child comes from
  `FLUX_EVAL_BINARY`/`RunContext`; the driver, dataset, and fixed rebuild decision come from documented
  trusted-host settings/defaults. A sentinel-key regression proves the credentialed argv contains
  only host-owned program/import selectors. Checked-in flows and benchmark runners use the host
  settings instead of embedding executable paths.

## Notes

- Evidence: primary review finding 2; the `run_with_env_exempt` contract already forbids
  model-selected executables.
