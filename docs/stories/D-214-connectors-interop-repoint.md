---
id: D-214
title: "Re-point the Zendesk reference flow at the flux-connectors Tool pack"
pillar: Agent
status: ready
priority: 2
epic: zendesk-automation
design: docs/designs/zendesk-automation.md
note: "the interop layer D-199 has been waiting for: flux-connectors registers a pack of dyn Tool via ClientBuilder::try_register_pack, each delegating to flux's OWN http.request — so flux keeps every byte of egress. Unblocks D-200/D-201/D-202"
---

# Re-point the Zendesk reference flow at the flux-connectors Tool pack

## Goal

Make `examples/zendesk.triage.flux` runnable again by pointing its `zendesk.*` operations at the
flux-connectors Tool pack, and unblock the three stories withdrawn with the plugin.

When `flux-plugin-zendesk` was removed in 0.38, the flow was retained deliberately as *"the authored
shape the replacement has to satisfy"*, with the note that **the op names are the part expected to
change and the flow structure is not**. This is that change.

## Acceptance

- [ ] A host can register connector operations with
      `ClientBuilder::try_register_pack(connector_pack::pack(&["zendesk"]))` and the four operations
      the flow calls resolve: `zendesk.test`, `zendesk.ticket.show`, `zendesk.ticket.search`,
      `zendesk.ticket.comment.list`.
- [ ] `examples/zendesk.triage.flux` loses its NOT-RUNNABLE header and runs all four entrypoints:
      `setup`, `triage`, `brief`, `eod`.
- [ ] **The flow's structure is unchanged** — four read-only entrypoints, retry with exponential
      backoff, bounded contexts with explicit budgets, model timeouts, deterministic evidence
      fallback. Only names moved.
- [ ] **It stays read-only.** No write operation is reachable from any of the four entrypoints; the
      pack's writes remain separately approval-gated. A test asserts this rather than a comment.
- [ ] Its provider-free coverage keeps passing — the tests drive the entrypoints against stubbed
      operations, and that must remain true so the shape stays honest without live credentials.
- [ ] `D-200`, `D-201`, `D-202` move off `blocked`, and `D-199`'s dependency note is closed or
      rewritten to whatever genuinely remains.
- [ ] Both workspace gates are green. A missing live credential is reported as a **skipped** smoke
      leg, never as simulated success.

## Notes

- The counterpart work is `flux-connectors` **C-113 – C-117**
  (`docs/designs/connector-tool-pack.md` in that repo). This story should not start until C-114 and
  C-115 have landed there, or there is nothing to point at.
- **The safety property to check when reviewing the pack, not to assume:** each generated Tool
  delegates to `HttpRequestTool::execute` directly, which **bypasses `Executor::dispatch`**. That
  means the inner call never consults `http.request`'s own `permission_subjects`
  (`crates/flux-web/src/http.rs:118`) or its `NetworkFetch` intent (`:126`). The connector Tools are
  required to mirror both. If they do not, installing a connector is a hole through this host's
  network policy — verify it on the pack rather than trusting the claim.
- Nothing here re-introduces a typed vendor plugin. The withdrawal decision stands; this is the
  generic layer that replaces it.
