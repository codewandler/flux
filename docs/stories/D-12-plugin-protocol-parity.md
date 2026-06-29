---
id: D-12
title: Plugin protocol parity extensions (auth / conn / blob)
pillar: Core
status: in-progress
priority: 1
design: docs/designs/plugin-protocol-parity.md
---

# Plugin protocol parity extensions (auth / conn / blob)

## Goal
Add the three **additive** host capabilities the remaining fluxplane plugins need so they can be written
natively on `host-kit` without doing privileged IO themselves: non-Bearer auth injection, a guarded raw
connection dialer, and a blob store. Clean extension of `flux.plugin.v1` — no fallback flags, the 8 shipped
plugins untouched. This is the prerequisite that gates D-15/D-16/D-17 and lets D-14 drop hand-rolled base64.

## Acceptance
- [x] **Slice A — auth.** `AuthMethod` carries `scheme: AuthScheme { Bearer|Basic|Header|Query }` (default
      `Bearer`) + `user_env` + `bearer`/`basic`/`header` constructors; `http.do` injects by `auth_purpose` per
      scheme (legacy `bearer_purpose` still works), secret never returned to the plugin. Test
      `auth_injection_resolves_per_scheme` asserts Basic = `base64(user:secret)`, Header/Query placement, and
      the legacy path. host-kit `Host::http`/`get_json`/`send_json` now send `auth_purpose`.
- [x] **Slice B — conn.** `flux_system::net::dial` (tcp/unix, reuses the `guard_url` egress policy) +
      `conn.dial/read/write/close` on `SystemHostCaps` over a per-scope tokio-mutex conn registry, gated by
      `PluginCapabilities.conn` (exact / single-`*` glob). Tests: `flux_system net::dial_tcp_round_trips_and_
      guards_private` + `flux_plugin conn_dial_round_trips_and_is_gated` (loopback echo; private/undeclared
      rejected). host-kit `Host::conn_dial`/`conn_read`/`conn_write`/`conn_close` + `ConnTarget`.
- [x] **Slice C — blob.** `blob.put/get/info` over a per-scope content-addressed (sha256) in-memory store,
      gated by `PluginCapabilities.blob`. Test `blob_put_get_info_round_trips_and_is_gated` (dedup by hash;
      unknown ref + ungranted denied). host-kit `Host::blob_put`/`blob_get`/`blob_info` + `BlobInfo`.
- [x] Gate green: `cargo test -p flux-plugin` (14), `cargo test --manifest-path plugins/Cargo.toml` (host-kit
      MockHost), clippy/fmt both workspaces, `flux-codegate` (dialer in flux-system L1; flux-plugin gained only
      external deps base64/sha2 — no new cross-layer flux edge).

## Progress
- **All three slices code-complete + gate-green, pending commit.** Host side in `crates/flux-plugin/src/lib.rs`
  (auth injection, `conn.*` registry, `blob.*` store; new deps base64 + sha2); the guarded dialer in
  `crates/flux-system/src/net.rs`; guest SDK in `plugins/host-kit/src/lib.rs` (auth_purpose wiring + `conn_*` +
  `blob_*` + MockHost echo/blob). All 8 plugins migrated to `..Default::default()` for the new `AuthMethod`/
  `PluginCapabilities` fields (behaviour unchanged — they stay Bearer; jira/confluence keep hand-rolled base64
  until D-14 switches them to the `Basic` scheme). **Deferred:** host-terminated TLS on `conn.dial`; a
  cross-invocation persistent blob dir + `flux plugin blob put` CLI; long-lived `process.start/stop/list`.

## Notes
- Design: [plugin-protocol-parity.md](../designs/plugin-protocol-parity.md). Epic:
  [fluxplane-plugins-parity.md](../designs/fluxplane-plugins-parity.md).
- Touch points: `crates/flux-plugin/src/lib.rs` (`AuthMethod`, `PluginCapabilities`, `SystemHostCaps::handle`),
  `plugins/host-kit/src/lib.rs` (`Host`), `crates/flux-system/src/net.rs` (new `dial`).
- Reuse: `resolve_purpose`/`resolve_endpoint`, `guard_http_url`/`flux_system::net::guard_url`,
  `truncate_on_char_boundary`. Deliberately **not** ported from fluxplane: provider-call, context providers,
  evidence observers, dual modes.
