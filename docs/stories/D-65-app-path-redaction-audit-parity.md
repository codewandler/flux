---
id: D-65
title: App-path redaction + audit parity — expose the App store/redactor seam, wire the dormant hooks
pillar: Core
status: done
design:
epic:
note: one root cause behind all four TODO(D-20/D-27/D-30) markers in flux-cli — `flux_app::App` owns its own store/bus, so the plugin-wiring sites have no EventStore/redactor in scope; the unwired cross-plugin secret sink is a (narrow) redaction-invariant gap
---

# App-path redaction + audit parity — expose the App store/redactor seam, wire the dormant hooks

## Goal
On the `flux app run` path the plugin/endpoint wiring cannot reach the per-run `EventStore` stream
or the executor's `Redactor` (`flux_app::App` owns its own store/bus), so four hooks that are live
on the `build_agent` path silently no-op: the cross-plugin credential **secret sink** (a resolved
credential is not seeded into the redactor — a narrow but real breach of the "secrets never appear
raw in model-visible output" invariant), the D-20 `PrivateNetAdmit` egress audit, the D-27
`CrossPluginResolve` audit, and the D-30 `EndpointDiscovered` audit. Give `App` a seam that exposes
both, wire all four hooks, and delete the TODOs.

## Acceptance
- [x] Failing-first test: a cross-plugin credential resolved on the app path is registered with the
      executor's redactor, so its raw value is scrubbed from model-visible tool results (mirror of
      the C-13 seeding guarantee, on this path).
- [x] `PrivateNetAdmit`, `CrossPluginResolve`, and `EndpointDiscovered` events are recorded on the
      app path with the run's stream/correlation identity — parity with the `build_agent` path,
      each pinned by a test.
- [x] The four `TODO(D-20)/(D-27)/(D-30)` comments at `crates/flux-cli/src/main.rs:1750/5215/5235/
      5252` are gone because the seams they describe are wired (not because the comments moved).
- [x] Explicitly out of scope, filed separately if wanted: the interactive first-use
      `CrossPluginApprover` — headless grant-only authorization is deliberate and unchanged.

## Progress
- **Seam**: `flux_app::App` gains an additive `App::with_events(..., events: Arc<EventStore>)`
  constructor (plus a matching `App::events() -> Arc<EventStore>` getter) that takes the host's
  `EventStore` explicitly instead of `Engine::new` always minting a fresh in-memory one.
  `App::with_sub_agents` now delegates to it with a fresh in-memory store, so every existing caller
  (flux-cli's `build_agent` path, the SDK, `strict_review_journey.rs`) is unaffected. This is the
  seam the story's Notes anticipated for the event-store half; the redactor half needed no new
  API — `run_app`'s own `redactor` local was already in scope at the plugin-wiring call site (only
  the `EventStore` was the real gap).
- **`crates/flux-cli/src/main.rs` (`run_app`)**: builds an in-memory `app_events: Arc<EventStore>` +
  one `app_run_stream` session id *before* the plugin/endpoint loop (mirroring the `build_agent`
  path's `events`/`session_id`), wires `.with_cross_plugin_audit(xplugin_audit)` on the broker
  (closes both the D-27 `CrossPluginResolve` and D-30 `EndpointDiscovered` audits — one hook, one
  broker call, per `flux_capabilities::CrossPluginAudit`'s two methods), and wires
  `.with_egress_audit(audit)` + `.with_secret_sink(secret_sink)` on each plugin's `SystemHostCaps`.
  The per-plugin caps assembly was extracted from an inline closure into a standalone function
  `app_plugin_caps(...)` (also `#[allow(clippy::too_many_arguments)]`, matching existing precedent
  in `flux-app`) so the wiring itself is directly unit-testable, not just structurally mirrored.
  `App::with_sub_agents(...)` at the bottom of `run_app` became `App::with_events(..., app_events)`
  so the wiring's own audit trail lands in the SAME store `App` uses for agent-target session memory
  and sub-agent spawn audit.
- **The build_agent-path residual**: `crates/flux-cli/src/main.rs:1750`'s `TODO(D-27)` (interactive
  `CrossPluginApprover`) is out of scope per this story — reworded to a `NOTE(D-27)` explaining the
  headless-grant-only posture is deliberate, not a gap, on *both* paths.
- **Tests** (all in `crates/flux-cli/src/main.rs`'s `mod tests` unless noted):
  - `flux-app`: `with_events_shares_the_given_store_not_a_fresh_one` — the new seam keeps the
    caller's store (`Arc::ptr_eq`), doesn't silently swap in a fresh one.
  - `egress_audit_adapter_records_private_net_admit_on_the_runs_stream` — direct test of
    `EventStoreEgressAudit` (shared by both paths).
  - `cross_plugin_audit_adapter_records_resolve_and_discovery_on_the_runs_stream` — direct test of
    `EventStoreCrossPluginAudit`'s two methods (shared by both paths).
  - `cross_plugin_credential_resolution_seeds_the_redactor_used_by_dispatch` — the acceptance
    centerpiece: drives `app_plugin_caps` (the exact function `run_app` calls) through a real
    cross-plugin credential resolution (`flux_capabilities::EndpointBroker` + a fake
    `CredentialReader`, mirroring flux-capabilities' own broker tests), then asserts a tool leaking
    that credential comes back scrubbed via the shared redactor. Verified this test actually catches
    a regression: temporarily dropped `.with_secret_sink(...)` from `app_plugin_caps` and confirmed
    the test fails with the raw secret in the output, then restored it.
  - Full gate: `cargo build/test/clippy -p flux-app -p flux-cli` all green (89 flux-cli + 25 unit +
    11 integration + 1 doctest flux-app tests), plus `cargo test -p flux-codegate` (layering) green.
    `cargo fmt -p flux-app -p flux-cli` applied (not `--all` — other agents have concurrent
    uncommitted work in other crates in this tree).
- **Scope discipline honored**: no `flux-capabilities` changes needed (no new adapter hooks — the
  existing `CrossPluginAudit`/`EgressAudit`/`SecretSink` traits already covered everything);
  authorization (grants/resolver seams) untouched.

## Notes
- Authorization is intact today: grants + resolver seams ARE wired; this story is parity of
  redaction + audit, not a bypass fix. Severity grounded against the envelope invariant
  (secret-redaction) per the review-grounding rule.
- Likely shape: `App` exposes (or accepts) its event stream + redactor at build time so the
  flux-cli wiring sites can hand them to `SystemHostCaps::with_egress_audit`,
  `with_cross_plugin_audit`, and the broker's secret sink.
