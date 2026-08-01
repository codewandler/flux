---
id: C-311
title: "Vendor-host disclosure at approval — show what an op reaches when flux is not the one dialing"
pillar: Core
status: in-progress
priority: 8
epic: connector-platform
areas: [flux-plugin, flux-runtime]
note: "the compensating control for the connectors seam's one real trade-off: when a platform dials the vendor, guard_url_scoped only ever sees localhost:8000, so flux's per-vendor egress allowlist stops constraining which vendor is reached"
---

# Vendor-host disclosure at approval — show what an op reaches when flux is not the one dialing

## Goal

When an operation's real network destination is reached by something *other* than flux — a connector
platform that injects the credential and calls the vendor itself — the operator must still be told
which vendor the call reaches, at the moment they are asked to approve it.

## Why this is not optional

The connectors seam's accepted design has the deployment execute the vendor call: flux sends
`{op, args}` to `localhost:8000` and never sees a vendor credential or a vendor URL. That is the right
credential boundary, and it costs something concrete:

**`guard_url_scoped` only ever sees `localhost:8000`.** flux's per-vendor egress allowlist — the
control that says "this agent may reach `api.zendesk.com` and nothing else" — stops constraining which
vendor is reached, because from flux's side every operation has the same destination. The platform's
own manifest becomes that control instead.

An approval prompt that says "call `connectors.zendesk-ticket-create`" while the operator cannot see
that this reaches `api.zendesk.com` is an approval given without the material fact. This story is the
compensating control that makes the trade-off defensible rather than merely accepted.

## Acceptance

- [x] **Failing-first test**: an approval request for an op whose manifest declares a vendor host
      carries that host in what the approver sees. It fails today because the declaration never
      reaches the approval path.
      → `crates/flux-plugin/tests/vendor_disclosure.rs::an_approval_for_a_platform_sourced_op_discloses_the_vendor_it_reaches`
- [x] The declared host is **re-verified host-side** against the manifest's `http_hosts` allowlist
      rather than trusted as free text on the individual op — a manifest that names a host outside its
      own declared allowlist is refused, and the test names that case.
      → `flux_plugin_protocol::validate_manifest_operations` (`crates/flux-plugin-protocol/src/lib.rs`),
      using the same `http_host_allows` matcher that gates real egress;
      `vendor_disclosure.rs::a_vendor_host_outside_the_manifests_own_allowlist_is_refused` and
      `lib.rs::a_vendor_host_must_be_inside_the_manifests_own_allowlist`
- [x] The disclosure appears on **every** approval surface that renders an op, not only the TUI —
      enumerate them and cover each, or state explicitly which are out of scope and why.
      → see "Approval surfaces" below
- [x] An op that declares **no** vendor host is disclosed as such rather than silently rendering as
      if it reaches nothing. "Unknown destination" and "no destination" must not look identical.
      → `VendorReach` is three-state; `vendor_disclosure.rs::an_undeclared_destination_never_renders_as_no_destination`
- [x] The disclosed value is redacted-safe: it must not become a channel for a token embedded in a
      URL by a hostile manifest.
      → the declared value must parse as a bare `host`/`host:port` (`split_vendor_host`), so a URL is
      unspellable rather than strippable; the refusal never quotes what it rejected.
      `a_vendor_host_that_is_a_url_is_refused_without_quoting_it`,
      `only_a_bare_host_may_be_declared_and_the_refusal_never_quotes_it`
- [x] Full gate green in both workspaces.

## Approval surfaces

The disclosure rides two existing channels, so no surface code changed:

| Surface | Channel | Covered |
|---|---|---|
| plain CLI / REPL per-op prompt (`StdinApprover`) | `Tool::permission_subjects` | yes |
| TUI approval sheet (`ChannelApprover` → `ApprovalView`) | `Tool::permission_subjects` | yes |
| whole-plan prompt, CLI (`plan_prompt`) and TUI (`plan_detail_lines`) | `PlanApprovalRequest::requirements` | yes — a `network.fetch` requirement |
| `PlanApprover` (flux-flow) | delegates to its interactive `fallback` | yes, via the fallback |
| `AllowApprover` / `DenyApprover` / `RiskApprover` / `SubAgentApprover` | both, rendered by neither | out of scope — headless, no operator to disclose to |
| `flux plugin call` | — | out of scope — an operator-initiated direct call that never reaches an `Approver` |
| the generated plugin skill doc (`plugin_skill.rs`) | — | out of scope — documentation, not an approval |

## Progress
- Filed 2026-07-31 from the approved connectors-seam plan.
- 2026-08-01 — implemented on `impl/C-311`.
  - Wire: `OperationSpec::reaches: VendorReach` (`Undeclared` | `Local` | `Host(String)`), additive
    and absent from the wire when undeclared, folded into the still-unpublished protocol `1.2.0` /
    host-kit `1.1.0` (crates.io max is `1.1.1` / `1.0.0`, so nothing shipped under those numbers).
  - Re-verification lives in `validate_manifest_operations`, which is the single choke-point run at
    load **and** at every refresh re-grant. `host_matches` moved into the protocol crate as
    `http_host_allows` so the load-time check and the runtime egress gate are one matcher.
  - `op_scope_weakenings` now refuses a refresh that sheds or re-points a disclosure.
  - Fixture: `platform_plugin` gained `discloses*` modes; the disclosing op is `dispatch` (present in
    every mode) so the failing-first test fails at the merge base for the story's reason rather than
    for a missing op.

## Notes
- Depends on nothing in [C-310](C-310-plugin-catalog-refresh.md) but shares its file
  (`crates/flux-plugin/src/host/loading.rs`) — the two should not run in the same wave.
- Precedent for pinning what a guard admitted: **C-256/C-257** bound fleet A2A, plugin HTTP/OAuth and
  plugin TCP to the exact DNS answers `guard_url_scoped` returned, disabled ambient proxies and
  automatic redirects, and re-authorized every supported redirect hop. The platform base URL still
  goes through that path; loopback is the easy case and must not become a special case.
- ⚠ C-309 changed `plugin_tool_spec` in the same file (`AccessKind::Process` is now unconditional).
