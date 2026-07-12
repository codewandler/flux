---
id: D-154
title: SDK `plugins` feature — subprocess plugin tools for embedders
pillar: Agent
status: done
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
- [x] Failing-first: a fixture subprocess plugin's op appears in `op_names()` and dispatches
      through the envelope — denied by the default approver, allowed with a rule.
- [x] Host capabilities stay manifest-scoped (`SystemHostCaps` defaults; no widening).
- [x] Lean default enforced: no flux-plugin in `cargo tree` without the feature.

## Progress
- **Done (unreleased).** New opt-in `plugins` cargo feature (`plugins = ["dep:flux-plugin"]`,
  `default = []`). `flux_sdk::plugins` (`crates/flux-sdk/src/lib.rs`) re-exports the plugin types and
  adds `load_tools(system, name, descriptor)` — wires **manifest-scoped** `SystemHostCaps` (no
  widening). `FlowClient::register_plugin(name, descriptor).await` (async; uses the client's System +
  mutable registry) and `ClientBuilder::with_plugin_tools(Vec<Arc<dyn Tool>>)` (pre-loaded tools ride
  the custom-op path) are the two doors.
- Cross-crate fixture problem (`CARGO_BIN_EXE_echo_plugin` is only exported to flux-plugin's own
  tests): solved with a feature-gated fixture binary `fixtures/plugin_fixture.rs` declared as a
  `[[bin]]` with `required-features = ["plugins"]` and a custom path (out of autobin discovery, so
  flux-sdk stays lib-only by default). The integration test `tests/plugins.rs` (`#![cfg(feature =
  "plugins")]`) reaches it via `CARGO_BIN_EXE_flux_sdk_plugin_fixture`.
- Tests (`tests/plugins.rs`): `op_names()` carries `fixture.upper`; with `auto_approve` it dispatches
  and uppercases; the default DenyApprover gates it (`Risk::Medium` — plugin ops default to Medium).
  Lean default: manifest test asserts `flux-plugin` optional; `cargo tree` = 0 default / 1 with feature.
- flux-sdk (L6) → flux-plugin (L4) codegate-legal; `flux-plugin` already precedes `flux-sdk` in the
  publish order (no script change). CHANGELOG + WHATS-NEW + website mirror updated. Gate green
  (workspace 2164; SDK all-features incl. the plugin integration test; clippy all-features / fmt /
  codegate). **Not committed/released.**

## Notes
- `flux_plugin::load_plugin_tools` (`crates/flux-plugin/src/lib.rs`); reuse the echo/caps test
  plugins (`crates/flux-plugin/src/bin/`) as fixtures. Plugin binaries are trusted dependencies,
  not OS-sandboxed code — say so in the rustdoc.
