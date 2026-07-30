# Zendesk automation

> **Status update — the plugin half of this design is withdrawn.** `flux-plugin-zendesk` was removed
> before its first release; a flux-connectors interop layer is to supersede it. The decision below is
> kept as the record of what was built and why, and because the parts that are *not* plugin-specific
> still hold: the authored-Flux control graph, the `--entry` command surface, the host-owned
> credential/endpoint split, and the rule that cognition may summarize evidence but never choose or
> parameterize a write. Read every "plugin" reference below as "the integration layer" — the
> replacement is expected to satisfy the same boundaries under different operation names.

## Decision

The first third-party automation reference is one checked-in Flux-Lang module with named flows,
backed by a first-party `zendesk` process plugin. Authored Flux owns the deterministic control graph;
the plugin owns typed Zendesk HTTP calls; optional cognition operations may summarize evidence but
never choose or parameterize writes.

The command surface is:

```text
flux run examples/zendesk.triage.flux --entry <setup|triage|brief|eod> [--inputs JSON] [--arg K=V]
```

`--entry` selects a top-level `flow` and executes it once through the same direct-flow engine used by
`flux flow run`. Without `--entry`, `.flux` files retain their existing app/program behavior.

## Trust boundaries

- The plugin performs no IO directly. It addresses `zendesk.endpoint`; the host resolves
  `ZENDESK_URL`, injects the `api_token` auth purpose, guards DNS/egress, and redacts the credential.
- Zendesk API-token auth uses Basic auth with `ZENDESK_USER=<email>/token` as the non-secret username
  and a token stored by `flux auth set zendesk` (or supplied as `ZENDESK_API_TOKEN`).
- Read operations are bounded to one page of at most 100 records. Pagination is explicit in results.
- Every ticket mutation requires the last observed `updated_stamp` and sends `safe_update=true`.
  Conflicts surface as failures; the plugin does not overwrite or retry stale state.
- Comment creation defaults to an internal note. Public replies require `public=true` and the op
  declares `send_external` in addition to its database write.
- The reference module calls read operations only. Ticket bodies and internal notes may enter the
  configured model context when the operator explicitly runs `triage`, `brief`, or `eod`; context is
  bounded and documentation calls out that exposure.

## Data flow

`setup` verifies credentials. `triage(query)` searches one queue page and asks for ticket-id-cited
priorities. `brief(ticket_id)` fetches the ticket and comments concurrently and asks for a factual
timeline and next action. `eod(query)` summarizes a bounded result set. Each cognition call is
deadline-bounded and wrapped in `fallback`, so provider absence or failure returns deterministic API
evidence rather than aborting the workflow.

Search results contribute `zendesk.ticket` datasource records. The initial plugin deliberately omits
ticket creation/deletion, bulk mutation, administration APIs, OAuth, uploads, and automatic pagination.

## Delivery slices

- L-92: one-shot named flow entrypoints on `flux run`.
- D-200: endpoint/auth plus test, search, show, and comment-list reads.
- D-201: safe update, comment, and additive-tag writes.
- A-136: the runnable four-entrypoint reference module.
- D-202: tutorial, catalog/release integration, changelogs, and full verification.

