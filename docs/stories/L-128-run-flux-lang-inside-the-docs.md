---
id: L-128
title: Run Flux-Lang inside the docs
pillar: Language
status: done
design: docs/designs/docs-workbench.md
note: Guarded scratch execution, shared editor, and fixed-root LSP for local docs.
---

# Run Flux-Lang inside the docs

## Goal
Turn the local documentation and console into one real Flux-Lang workbench: authored examples can
be edited, checked, and—where the page declares a safe fixture—executed through Flux's ordinary
safety envelope. Keep the hosted documentation useful without implying that it has a runtime.

## Acceptance
- [x] `public_docs::non_loopback_docs_never_construct_or_mount_a_runtime` proves public binds expose
  only static docs and structural projection; loopback binds advertise execution only after the
  launch-secret exchange.
- [x] `public_docs::run_is_scratch_scoped_and_approval_bound` proves an eligible example executes
  under an isolated scratch workspace and effectful calls cannot bypass fingerprint-bound approval.
- [x] `public_docs::undeclared_examples_cannot_be_executed` proves arbitrary page source cannot turn
  the documentation server into a general host-workspace execution surface.
- [x] `protocol::fixed_workspace_ignores_a_browser_supplied_root` proves the browser LSP can never
  select a host path while the stdio server retains its normal client-root behavior.
- [x] The console and Flux code blocks use one lazy Monaco-based `FluxWorkbench`, with Flux syntax,
  LSP diagnostics/completion/hover/formatting, graph projection, inputs, streamed output,
  approvals, cancellation, and scratch-file views as capabilities allow.
- [x] `language/examples` declares the five safe runnable fixtures and explains why the remaining
  examples are edit/check-only; `tutorial/first-app` exposes the two tutorial app variants as
  persistent, restart-required browser sessions with their tutorial files.
- [x] `flux docs --model <spec>` resolves a model lazily; omitting it uses normal Flux model
  resolution. The hosted website remains edit/highlight-only.
- [x] CLI, server, LSP, SDK/app, website, and bundle-contract tests pass; release notes and both
  documentation changelog mirrors describe the user-visible result.

## Progress
- 2026-08-02: Story and design contract recorded; implementation started from completed L-127.
- 2026-08-02: Shipped the shared Monaco/LSP workbench, guarded flow and app sessions, declared
  cookbook/tutorial fixtures, lazy docs model selection, and the release-matched embedded bundle.

## Notes
- Extends L-127 and `docs/designs/embedded-docs-playground.md`; it does not weaken the original
  public-bind safety claim.
- No shell, plugin, host-workspace, secret, private-network, or sub-agent capability is part of the
  docs runtime.
