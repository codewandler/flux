---
id: D-199
title: "Zendesk automation — deterministic support workflows with bounded AI (epic)"
pillar: Agent
status: in-progress
epic: zendesk-automation
design: docs/designs/zendesk-automation.md
areas: [flux-cli, flux-lang, plugins, docs]
note: "EPIC — L-92, A-136, D-200, D-201, D-202 all closed; D-214 landed flux's half of the connector-pack interop. Open on ONE external dependency: two flux-connectors gaps (no zendesk `authority`; `{subdomain}` unresolved) that no flux change can close"
---

# Zendesk automation — deterministic support workflows with bounded AI (epic)

## Goal

Ship the first complete reference for deterministic third-party automation in Flux-Lang: configure
one Zendesk API token, select a named workflow from one `.flux` file, run typed Zendesk calls through
the plugin safety envelope, and optionally use a model for bounded analysis without granting it write
control.

## Acceptance

- [x] L-92, D-200, D-201, A-136, and D-202 are done with their named failing-first tests — D-200,
      D-201 and D-202 closed under D-214 rather than re-implemented; see each story for which of its
      bullets carried over, which were superseded, and which turned out **void**.
- [ ] ~~A local install can run `setup`, `triage`, `brief`, and `eod`~~ → **the one open bullet, and
      it is not flux work.** The integration exists and serves these exact operation names; two
      flux-connectors gaps keep a live run impossible — the Zendesk connector declares no `authority`
      (so there is no credential address to resolve) and its `https://{subdomain}.zendesk.com` base
      URL is not resolved from config. Both refuse rather than sending a broken request. No change in
      this repository can close this bullet; it is tracked on D-214 and nowhere else.
      Model failure preserving deterministic output **is** proven offline
      (`crates/flux-eval/tests/zendesk_triage.rs` drives a failing-cognition run and asserts the
      authored fallback returns the already-gathered evidence).
- [x] The reference workflow contains no Zendesk write operation — asserted against the module's own
      call graph, and its operation set now pinned **exactly** rather than by prefix (D-214).
      ~~while the plugin's separately callable writes are…~~ → **restated:** there is no plugin and no
      separate call surface. The writes are declared typed, gated and concurrency-safe in the
      connector catalogue (`safe_update` const `true`, required `updated_stamp`, `conditional`
      idempotency), but registering the pack brings them into the same registry as the reads, so the
      boundary is approval and policy rather than registry absence. That is a weaker default than the
      plugin's and is documented as such.
- [x] Both root and nested plugin workspace gates are green. ~~unavailable live credentials are
      reported as a skipped smoke leg~~ → **void:** no smoke leg exists to skip, and with no
      credential *address* declared, reporting a skip would falsely imply that a credential is the
      only thing missing.

## Progress

- 2026-07-30 — epic and implementation stories filed; design locked in
  [zendesk-automation.md](../designs/zendesk-automation.md).
- 2026-07-30 — L-92, D-200, D-201, and A-136 are implemented and done. D-202's documentation,
  catalogs, smoke, and release note are done; the epic stays in progress until unrelated concurrent
  root-gate failures clear and a separate signed plugin-pack release is cut.
- 2026-07-30 — D-202 is closed: the concurrent remediation work landed and both workspace gates are
  green on the integrated tree. Two acceptance bullets remain, and neither is source work: the
  documented `setup`/`triage`/`brief`/`eod` run needs live Zendesk credentials (the smoke leg skips
  honestly without them), and the signed plugin-pack release carrying `flux-plugin-zendesk` is cut
  separately from the core release. The epic stays open on those two.
- 2026-07-31 — **one of those two was wrong and the other is void.** The plugin-pack debt is
  discharged by deletion: the binary was removed before it ever shipped, so nothing is owed. And the
  live run does **not** merely need credentials — D-214's investigation found two structural gaps in
  flux-connectors, either of which alone prevents a request from reaching Zendesk: no `authority` on
  the connector means there is no address to look a credential up at, and an unresolved
  `{subdomain}` means the URL names a host that does not exist. A skipped smoke leg would have
  reported "no credentials" for a failure that credentials cannot fix.
  D-214 landed flux's half — the pack already speaks this flow's exact operation names, so the flow's
  body did not change; what landed is the exact-set pin that keeps it so, and the correction of four
  documentation blocks that had decayed into instructing readers to install a removed binary. The
  epic now has exactly one open bullet, owned by another repository.
