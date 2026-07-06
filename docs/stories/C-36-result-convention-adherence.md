---
id: C-36
title: "Error/Result convention adherence — codify the wire-seam exception, convert stragglers"
pillar: Core
status: done
note: "god-review finding #16 (the one confirmed multi-crate observation): AGENTS.md declares `flux_core::Result` for library crates, but 32 bare `Result<_, String>` signatures + a private a2a alias + rusqlite::Result leaks deviate — some deliberately (wire seams), some not"
---

# Error/Result convention adherence — codify the wire-seam exception, convert stragglers

## Goal
AGENTS.md already states the convention ("library crates return `flux_core::Result<T>` /
`flux_core::Error`; the `flux` binary uses `anyhow`"), but adherence drifted: ~32 bare
`Result<_, String>` signatures across flux-plugin, flux-a2a, flux-capabilities, flux-lang's CLI
bin, and flux-cli; flux-a2a keeps its own `Result` alias; flux-events exposes `rusqlite::Result`
in at least one signature. Some of these are **deliberate** — plugin-protocol and JSON-RPC errors
cross the wire as strings — but that exception is nowhere written down, so every review re-flags
it. Classify every deviation, convert what should follow the convention, and codify the legitimate
exception in AGENTS.md so the convention is checkable.

## Acceptance
- [x] AGENTS.md's Errors bullet gains the wire-seam exception in one or two sentences: where
      errors are protocol payload (plugin frames' `err`, a2a JSON-RPC error objects, host-cap
      callback results), `String` errors at the seam are correct — internal helpers feeding those
      seams may keep `Result<_, String>`.
- [x] Every bare `Result<_, String>` / `Result<String, String>` in `crates/` is either (a)
      converted to `flux_core::Result` (or the binary's `anyhow`), or (b) demonstrably a wire-seam
      case covered by the documented exception. The classification is recorded in this story's
      Progress (file:line → converted / wire-seam). The one flagged non-wire-seam straggler,
      `flux-tools/src/extra.rs:300`, was converted by the orchestrator in the same pass (see the
      follow-up Progress entry) — nothing left unconverted or undocumented.
- [x] `crates/flux-plugin` is classified-only, NOT edited — its String errors are the protocol
      frame seam, and D-54 owns that crate concurrently.
- [x] flux-events: no `rusqlite::Result` in public signatures (internal helpers may keep it).
- [x] Style survivor #12 from the same review: `new_id` gets its own `pub use` line in
      `crates/flux-a2a/src/lib.rs` instead of hiding inside the type re-export block.
- [x] Pure refactor: no behavior change; full existing test suite green; clippy/fmt clean in the
      touched crates.

## Progress
- 2026-07-06 filed — from the god-review validation pass (`review.md`, findings #16 + #12).
- 2026-07-06 implemented. Scope was constrained to a file boundary set by the orchestrator (concurrent
  agents own other crates): editable this pass were `flux-a2a`, `flux-capabilities`, `flux-events`,
  `flux-lang/src/bin/fluxlang.rs`, `flux-cli`, and this AGENTS.md bullet. `flux-plugin` was explicitly
  classify-only (D-54 owns it concurrently); `flux-secret`, `flux-tools`, `flux-server`, `flux-flow`,
  and `flux-lang/src/host.rs` turned out to also carry hits but were outside the editable set, so they
  are classified-only below too — full conversion is a fast-follow for a story with those crates in
  scope.

  **AGENTS.md** — extended the Errors bullet with the wire-seam sentence quoted above (plugin frame
  `err`, A2A JSON-RPC error object, `HostCapabilities`/`ReferenceResolver` callback result).

  **Converted (this pass):**
  | File:line | Was | Now |
  |---|---|---|
  | `flux-capabilities/src/endpoint/mod.rs:106` `EndpointRegistry::import` | `Result<EndpointRef, String>` | `flux_core::Result<EndpointRef>` |
  | `flux-capabilities/src/endpoint/mod.rs:119` `EndpointRegistry::load` | `Result<(), String>` | `flux_core::Result<()>` |
  | `flux-capabilities/src/endpoint/mod.rs:143` `EndpointRegistry::save` | `Result<(), String>` | `flux_core::Result<()>` |
  | `flux-lang/src/bin/fluxlang.rs` `run`/`read_source`/`render_ast`/`compile_text` + 2 test helpers | `Result<_, String>` | `flux_core::Result<_>` (flux-core was already a dependency; no new dep added) |
  | `flux-cli/src/main.rs:3272` `run_pending_plan`'s local `result` binding | `Option<Result<String, String>>` | `Option<anyhow::Result<String>>` |
  | `flux-cli/src/main.rs:5945` `coerce_arg_value` | `Result<Value, String>` | `anyhow::Result<Value>` (test at ~7435 updated: `err.contains(...)` → `err.to_string().contains(...)`, semantics unchanged) |
  | `flux-a2a/src/lib.rs:22-28` `new_id` re-export | hid inside `pub use types::{...}` | own `pub use types::new_id;` line (style #12) |

  **Classified as wire-seam, kept `Result<_, String>` (in-boundary, documented inline where new):**
  | File:line | Why |
  |---|---|
  | `flux-capabilities/src/endpoint/mod.rs:191` `StaticResolver::materialize` | private helper feeding the `ReferenceResolver` impl below — comment added |
  | `flux-capabilities/src/endpoint/mod.rs:220,236` `StaticResolver::{resolve_endpoint,resolve_credential}` | implement `flux_plugin::ReferenceResolver`, a fixed external trait signature — comment added |
  | `flux-capabilities/src/endpoint/host_caps.rs:54` `EndpointBrokerHostCaps::handle` | implements `flux_plugin::HostCapabilities` — literal host-capability callback result |
  | `flux-capabilities/src/endpoint/host_caps.rs:119,166` | test fakes for the above (`ProviderInvoker`/`HostCapabilities` doubles) |
  | `flux-capabilities/src/datasource/host_caps.rs:36` `DatasourceHostCaps::handle` | implements `flux_plugin::HostCapabilities` |
  | `flux-capabilities/src/endpoint/broker.rs:162,188,277,294,572,610,720,732,772,793,808` + test fakes at `873,1084,1184,1279,1619` | `ProviderInvoker`/`CredentialReader` (plugin subprocess call seams) and the `ReferenceResolver` impl + its private `authorize_cross_plugin`/`materialize_cross_plugin` helpers — all either implement the external trait or exist only to feed it |
  | `flux-capabilities/src/endpoint/ops.rs:362` | test fake `ProviderInvoker::discover` |
  | `flux-a2a/src/server.rs:28,34` `A2aTurn::{run,run_rich}` trait + `223,233,255,258` test impls | already documented in-file: "Errors are returned as a message string so the dispatcher can surface them as a JSON-RPC error" |
  | `flux-a2a/src/client.rs:37` local `pub type Result<T> = Result<T, A2aError>` | not a `Result<_,String>` stray — a normal per-crate typed error alias; kept as-is per this story's notes |
  | `flux-events/src/store.rs:56` `row_to_summary` | private, forced by rusqlite's `query_map`/`query_row` callback signature (`Fn(&Row) -> rusqlite::Result<T>`); every `pub fn` in the crate already returns `flux_core::Result` — acceptance already satisfied, no edit needed |
  | `flux-capabilities/src/datasource/sqlite.rs:245` | a local `.collect::<rusqlite::Result<_>>()` turbofish, `.map_err(map_sql)`'d immediately after — not a signature, not an issue |

  **Classified only, NOT edited (outside the file boundary for this pass):**
  | Crate | Hits | Classification |
  |---|---|---|
  | `flux-plugin` (pg.rs, lib.rs, hooks.rs, bin/echo_plugin.rs, bin/caps_plugin.rs, tests/host.rs — ~35 hits) | all | wire-seam: `GuestHost`/`PluginHandler`/`ReferenceResolver`-style trait defs+impls, the Postgres handshake driven over the plugin subprocess, and the host-capability resolver helpers (`resolve_purpose`, `resolve_endpoint`, `guard_http_url`, `parse_credential_ref`, …) that feed them. Per the task's explicit instruction this crate is classify-only; D-54 (guest SDK malformed-frame handling) is being implemented in it concurrently, confirmed live during this session. |
  | `flux-secret/src/lib.rs:77` `Ref::parse` | wire-seam-adjacent | public parse helper; its only non-test caller is flux-plugin's wire-seam `parse_credential_ref` |
  | `flux-lang/src/host.rs:101,132` `OpHost::resolve_thing` + `default_resolve_thing` | host-seam | the flux-lang interpreter ↔ host trait boundary (flux-flow's `ExecutorHost` on the other side); doc comment: "the error string surfaces to the flow as a runtime error" — same shape as a host-capability callback, but out of boundary (only `bin/fluxlang.rs` was authorized in flux-lang) |
  | `flux-flow/src/compile.rs:784,1357` `parse_draft_ast` + the `emit_plan` decode arm | model-wire | parses the model's raw tool-call JSON; the error string is fed back to the model as repair/retry text — same spirit as the wire-seam exception, on the model side instead of the plugin/A2A side |
  | `flux-server/src/a2a.rs:279` `subscribe` (SSE `message/stream`) | wire-seam | downstream half of the same A2A JSON-RPC error protocol as `flux-a2a/src/server.rs` |
  | `flux-tools/src/cognition.rs:820` `parse_finding` | message-is-the-payload | private helper whose `Err(String)` is pushed verbatim into a human-readable `gaps: Vec<String>` diagnostics list — the string is the deliverable, not an internal error |
  | `flux-tools/src/extra.rs:300` `sqlite_query`'s `spawn_blocking` closure | **genuine straggler, not wire-seam** | a private closure immediately `.map_err(Error::Other)`'d after `.await` — trivially convertible to `flux_core::Result`, just outside this pass's boundary. **CONVERTED by the orchestrator same-day, see the follow-up entry below.** |

  Gate (package-scoped): `cargo build`/`test`/`clippy --all-targets -- -D warnings` all green for
  `flux-a2a`, `flux-capabilities`, `flux-events`, `flux-lang` (`--features cli`), `flux-cli`;
  `cargo fmt --all` then `-- --check` clean. `flux-codegate` layering test green (no crate/dependency
  changes). Concurrent, unrelated uncommitted work from other sessions was observed landing on disk
  mid-task in `flux-core`, `flux-orchestrate`, `flux-plugin`, and `docs/usage.md` (stories A-41 and
  D-54) — left untouched; `cargo fmt --all -- --check` confirms it was already fmt-clean, so this
  story's `cargo fmt --all` run made no changes to those files.
- 2026-07-06 (orchestrator follow-up, after all concurrent agents finished) — converted the one
  flagged non-wire-seam straggler: `crates/flux-tools/src/extra.rs` `sqlite_query`'s
  `spawn_blocking` closure now returns `flux_core::Result` (each `map_err` wraps `Error::Other`;
  the caller's trailing `.map_err(Error::Other)?` collapsed to `??`). No behavior change; covered
  by the full-workspace gate run for the A-41/D-54/C-36 batch.

## Notes
- Evidence inventory (from validation): `flux_core::Result` alias at `crates/flux-core/src/error.rs:7`;
  bare hits in flux-plugin (pg.rs, lib.rs — wire-seam), flux-a2a (`server.rs:28,223,233,255` —
  JSON-RPC seam; local alias `client.rs:37`), flux-capabilities (`endpoint/mod.rs`, `endpoint/broker.rs`),
  flux-lang (`bin/fluxlang.rs`), flux-cli (`main.rs:3272`); `rusqlite::Result` at
  `crates/flux-events/src/store.rs:56`.
- flux-a2a's local `Result<T, A2aError>` alias is a normal per-crate error type — keep it, but the
  re-export block hygiene (#12) applies.
