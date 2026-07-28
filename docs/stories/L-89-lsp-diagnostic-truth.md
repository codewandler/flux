---
id: L-89
title: Diagnostic truth — workspace composites in the catalog, real severities, stable codes
pillar: Language
status: backlog
epic: flux-lsp-round-2
design: docs/designs/flux-lsp-round-2.md
note: the LSP catalog stops at the file edge (authoring_registry:27, signatures_for_document:79) so a call to a composite stored in .flux/flows — which DynamicComposites::load installs for the real host (flux-flow/src/composites.rs:100) — is flagged "unknown operation"; and every analyzer finding is emitted as a bare WARNING with no code (lsp_warning:553)
---

# Diagnostic truth — workspace composites in the catalog, real severities, stable codes

## Goal

Stop the editor disagreeing with the runtime: a flow that runs must not squiggle, a flow that cannot
run must squiggle as an error, and every finding must carry a code a client can filter on.

## Why (evidence)

- `authoring_registry` (`crates/flux-lsp/src/main.rs:27-46`) registers built-ins + cognition +
  datasource + web. `signatures_for_document` (`main.rs:79-91`) adds composites declared **in the
  same buffer**. Nothing reads the composites the host installs from disk —
  `flux_flow::composites::DynamicComposites::load` (`crates/flux-flow/src/composites.rs:100`) loads
  `.flux/flows` (`:113`) and the `@global_flows` root (`:108`), the same home `flow_list`/`flow_run`
  use (`crates/flux-tools/src/flows.rs:26`).
- Consequence: a `.flux` file calling a project composite gets the "unknown operation" warning
  (`genuinely_unknown_operation_stays_a_warning`, `main.rs:1545`) while `flux flow run` executes it
  happily. A false positive in the one surface whose job is to be trusted.
- `lsp_warning` (`main.rs:553-561`) stamps `DiagnosticSeverity::WARNING` on *every* analyzer
  finding, with no `code`. A composite cycle (`main.rs:1554`), an unbound symbol (`:1565`), and a
  wrong argument count (`:1576`) — all conditions that make the flow un-runnable — render
  identically to a soft hint. Parse errors are the only `ERROR`s (`cst_diagnostics`, `main.rs:589`).

## Acceptance

- [ ] The authoring catalog includes composite ops discovered in the workspace flow home
      (`.flux/flows`, `.flux/ops`, and the global roots) via the existing loader, so calling one is
      not reported as unknown.
- [ ] Discovery is read-only, goes through `flux_system::System`, and does not turn editor startup
      into a workspace crawl — load once, refresh on a `didSave`/watched-file change rather than per
      keystroke; an unparseable file in the flow home is skipped, never fatal (matching
      `load_flows_dir`'s lenient contract, `composites.rs:298-302`).
- [ ] Findings that make a declaration un-runnable (unknown op, unbound symbol, arity/type mismatch,
      composite cycle, duplicate op) are emitted as `ERROR`; advisory findings stay `WARNING`.
- [ ] Every diagnostic carries a stable `code` (plus `source: "flux-lsp"`, already set).
- [ ] Failing-first tests: (a) a buffer calling a composite defined in `.flux/flows` produces no
      unknown-operation diagnostic, while an genuinely undefined op still does; (b) an unbound
      symbol is reported with `ERROR` severity and a code; (c) a broken file in the flow home is
      skipped without failing the whole document's diagnostics.

## Progress
- (not started)

## Notes
- This is the epic's only new IO — keep the `authoring_registry` invariant intact (catalog-only, no
  model/network/credential IO at startup; `main.rs:32-34`).
- Unlocks cross-file go-to-definition and `workspace/symbol` as a follow-up; both need the same
  index. File them separately rather than widening this story.
