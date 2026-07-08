---
id: D-96
title: Ephemeral --allow-private-net CLI egress override
pillar: Core
status: done
priority:
epic:
design: docs/designs/scoped-private-net-egress.md
note: "global --allow-private-net = this-process-only private-net grant (no config edit); widens the operator-grant side only, so the manifest-hosts intersection + PrivateNetAdmit audit (grant_source cli:--allow-private-net) are preserved; web_fetch opened for the run; extends D-20, advances D-95 ergonomics; DONE, UNCOMMITTED"
---

# Ephemeral --allow-private-net CLI egress override

## Goal
Let an operator reach a private/internal endpoint for a single invocation
(`flux --allow-private-net plugin call gitlab …`) without editing `config.toml`, as the ephemeral,
audited equivalent of a `[private_net]` grant. Extends the [D-20](D-20-scoped-private-net-egress.md)
scoped model and advances the operator-ergonomics half of
[D-95](D-95-direct-call-private-net-parity.md) (a safe way to test a private endpoint).

## Acceptance
- [x] A global `--allow-private-net` flag on the `flux` binary, propagated via `FLUX_ALLOW_PRIVATE_NET`
      so surfaces that don't receive `Cli` (notably `flux plugin call`) observe it.
- [x] It widens **only the operator-grant side** to `*` at every egress-wiring site (web_fetch, the
      agent plugin loop, `app run`, and `plugin call`); `SystemHostCaps::private_net_allow` still
      intersects with each plugin's manifest-declared `private_hosts`, so a plugin declaring none stays
      refused — deny-by-default is preserved (not a bypass path).
- [x] Admissions on the agent/app paths audit with a distinct `grant_source = "cli:--allow-private-net"`
      (`web_fetch` and the thin `plugin call` path have no audit sink today — unchanged).
- [x] `web_fetch` is opened to private ranges for the run (no manifest safeguard) — documented with the
      SSRF caveat; a scoped `[private_net.plugins]` grant is preferred for recurring use.
- [x] Test: `allow_private_net_override_widens_grant_and_labels_audit` (flux-cli) proves the widening
      + audit label under the env, and deny-by-default when off; the existing egress-audit test's
      `config:plugin/…` source stays green.

## Progress
- **Done, uncommitted.** `crates/flux-cli/src/main.rs`: the `Cli` flag, the `FLUX_ALLOW_PRIVATE_NET`
  export in `apply_workspace_access_env`, and the helpers `private_net_cli_override` /
  `effective_plugin_private_hosts` / `effective_web_fetch_private_hosts` / `private_net_grant_source_for`
  wired into the four egress sites. Docs in `website/docs/reference/config.md`; CHANGELOG `[Unreleased]`.
- Gate: flux-cli bin builds + clippy(bins) clean; the flag unit test passes; `flux-codegate` green.
  (The full `cargo test -p flux-cli` is currently blocked by an unrelated concurrent-session file,
  `crates/flux-cli/src/usage.rs`, whose test references the private `style::COLOR`.)

## Notes
- Honors AGENTS.md "there are no bypass paths": the flag is an ephemeral *grant*, the same trust
  decision as a config grant, and every request still traverses `guard_url_scoped`.
- Touch points: the four wiring sites in `crates/flux-cli/src/main.rs`; the grant-source override uses
  `flux_plugin::SystemHostCaps::with_grant_source`.
- Follow-up: wiring a `PrivateNetAdmit` audit sink into the direct `plugin call` path (its thinness is
  the D-95 gap); [D-98](D-98-flux-web-plugin-and-http-request-op.md) re-homes `web_fetch` under normal
  plugin limits, after which its special native private-net path can retire.
