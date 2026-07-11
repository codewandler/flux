---
id: D-139
title: Structured node path on Diagnostic — a typed field instead of the rendered locator suffix
pillar: Language
status: done
epic:
design:
note: "downstream ask (ai-agent-platform flows arc, ask 5): the platform's NodeMap parses the rendered `(at `body[2].then[0]`)` message suffix to key diagnostics to canvas nodes; a typed field removes that string coupling (canary-tested downstream; graceful degradation to flow-level)"
---

# Structured node path on Diagnostic — a typed field instead of the rendered locator suffix

## Goal
`Diagnostic` carries its node path (`body[2].then[0]`) as a **typed field**, not only as a rendered
`` (at `…`) `` suffix inside the message string. Downstream consumers that attribute diagnostics to
their own source model (ai-agent-platform's `NodeMap` keys analyzer findings back to graph-canvas
nodes) currently parse the message text — canary-tested, but a string contract where a struct field
belongs.

## Acceptance
- [x] `Diagnostic` gains an optional structured path (segment list or the canonical string form),
      populated wherever the analyzer renders the `(at …)` locator today.
- [x] The rendered message keeps the suffix (human-facing contract unchanged) — the field is
      additive.
- [x] Failing-first test: a node-scoped analyzer finding exposes the same path through the field
      as through the rendered suffix.

## Progress
- 2026-07-12 — filed from the ai-agent-platform flows arc (flows.md upstream ask 5). Nothing gates
  on it (the downstream parse is canary-tested and degrades to flow-level attribution).
- 2026-07-11 — implemented: `Diagnostic` (`crates/flux-lang/src/analyze.rs`) gains
  `pub node_path: Option<String>`. The one choke point that renders the `` (at `…`) `` suffix
  (`Diags::add`) now derives `node_path` from the same `self.path.join(".")` used to render the
  suffix and stores both on the pushed `Diagnostic`, so every node-scoped finding across the
  analyzer (structural walk, type-checking pass, `analyze_call`) populates it identically —
  there is no second rendering site to update. `Diagnostic::new` (the path-less constructor used
  by `analyze_call`'s flow-level "unknown operation" error and in `context_slice.rs` tests) keeps
  `node_path: None`. Added failing-first test `diagnostic_node_path_matches_rendered_suffix`
  (watched it fail to compile — `no field \`node_path\`` — before implementing) asserting a
  nested `when`-branch finding's `node_path` (`Some("body[0].then[0]")`) matches the path embedded
  in its rendered message suffix. Checked every `Diagnostic`-touching site in the workspace
  (`flux-lsp/src/main.rs`, `flux-lang/tests/analyzer_ranges.rs`, `flux-flow`, `flux-sdk`,
  `flux-cli`) — none construct `Diagnostic` via struct literal outside `analyze.rs`, so the new
  field is purely additive and every consumer keeps compiling unchanged. Gate:
  `cargo test -p codewandler-flux-lang` (341 lib + all integration suites green except the
  pre-existing, unrelated `website_customer_changelog_is_in_sync` drift from concurrent D-138
  WHATS-NEW.md work), `cargo test -p flux-lsp` (14/14), `cargo test -p codewandler-flux-flow`
  (301+3+1 green), `cargo test -p codewandler-flux-sdk` (green), `cargo clippy -p
  codewandler-flux-lang --all-targets -- -D warnings` and `cargo clippy -p flux-lsp --all-targets
  -- -D warnings` clean, `cargo fmt -p codewandler-flux-lang -- --check` clean. No serde derives
  added — `Diagnostic` isn't serialized anywhere in the workspace today, so a plain field is the
  minimal additive shape.
