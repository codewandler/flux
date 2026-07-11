---
id: D-154
title: SDK `plugins` feature — subprocess plugin tools for embedders
pillar: Agent
status: backlog
epic: sdk-surface
design: docs/designs/sdk-surface.md
note: "wave 3 — load_plugin_tools into the same gated registry; flux's MCP answer"
---

# SDK `plugins` feature — subprocess plugin tools for embedders

## Goal
Behind a `plugins` cargo feature (dep: flux-plugin), `ClientBuilder::with_plugin_tools(...)` /
`FlowClient::register_plugin(...)` wrap `flux_plugin::load_plugin_tools` so installed plugins'
operations become policy-gated tools inside an embedded agent — same envelope, same approval
path.

## Acceptance
- [ ] Failing-first: a fixture subprocess plugin's op appears in `op_names()` and dispatches
      through the envelope — denied by the default approver, allowed with a rule.
- [ ] Host capabilities stay manifest-scoped (`SystemHostCaps` defaults; no widening).
- [ ] Lean default enforced: no flux-plugin in `cargo tree` without the feature.

## Progress
- (pending)

## Notes
- `flux_plugin::load_plugin_tools` (`crates/flux-plugin/src/lib.rs`); reuse the echo/caps test
  plugins (`crates/flux-plugin/src/bin/`) as fixtures. Plugin binaries are trusted dependencies,
  not OS-sandboxed code — say so in the rustdoc.
