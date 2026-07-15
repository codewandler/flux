---
id: C-77
title: Pin egress connections to the guard-vetted IP (close DNS-rebinding)
pillar: Core
status: done
priority: 2
epic: harness-hardening
design: docs/designs/harness-hardening.md
note: "SECURITY (Critical) — guard vets resolved IP, reqwest re-resolves at connect → cloud-metadata theft"
---

# Pin egress connections to the guard-vetted IP (close DNS-rebinding)

## Goal
Eliminate the TOCTOU between the SSRF guard and the actual connection: `guard_url_scoped` resolves the
host, rejects private answers, then hands the *hostname* to reqwest, which re-resolves at connect with
no pinning. A low-TTL attacker host answers public to the guard and `169.254.169.254` to connect,
stealing cloud-metadata credentials via `web.fetch`/`web.crawl`/`http.request`/`browser.*`.

## Acceptance
- [x] Failing-first test (`net::tests::pinned_guard_returns_the_vetted_address_to_pin_the_connection`,
      injected `HostResolver`): the guard returns the exact vetted addr to pin to, so a public answer
      to the guard pins the connection to that public addr — a later internal answer is never dialed.
- [x] The per-request client connects only to the vetted IP set — `pinned_client` builds a per-hop
      reqwest client with `ClientBuilder::resolve_to_addrs(host, vetted)`; no connect-time re-resolution.
- [~] Consumers: `http.request`, `web.fetch`, `web.crawl` DONE (all pin via `guard_url_scoped_pinned`,
      incl. every redirect hop). `browser.*` connects through CDP/chromiumoxide, not this reqwest path,
      so it keeps the pre-connect `guard_url_scoped` check only — pinning it is tracked as residual.

## Progress
- **2026-07-15 — DONE (compile + unit-test verified; full gate pending).** Added
  `guard_url_scoped_pinned[_with_resolver]` in `flux-system::net` (returns the vetted `Vec<SocketAddr>`
  alongside the URL, sharing one `guard_and_pin` core). `egress::send_guarded` now pins each hop via
  `resolve_to_addrs`; `GuardedRequest` carries the pin; unresolvable hosts fall back to the shared
  client. 114 flux-web + 56 flux-system tests pass. Residual: `browser.*` (CDP path).

## Notes
- `crates/flux-system/src/net.rs:114` (`guard_url_scoped`); consumers in `crates/flux-web/src/egress.rs:65,153`.
  The module comment already concedes rebinding is unclosed.
- Design: [harness-hardening](../designs/harness-hardening.md).
