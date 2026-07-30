# Zendesk triage workflow

> **The backing integration is being replaced, so this workflow cannot run today.** The
> `flux-plugin-zendesk` plugin was **removed before its first release** — it is to be superseded by a
> flux-connectors interop layer. No signed pack was ever published, so nothing was withdrawn from
> users. The reference flow is kept, and so is this page: the control-flow shape and the data-exposure
> boundary below are the contract the replacement has to satisfy. The `zendesk.*` operation names are
> the part expected to change. The setup steps are retained for that reason and will not work until
> the replacement lands.

The reference flow is [`examples/zendesk.triage.flux`](../examples/zendesk.triage.flux). It
demonstrates a deterministic third-party workflow: Flux-Lang fixes the control graph and literal
operation names, the integration performs typed API calls through guarded host capabilities, and AI is
confined to bounded analysis steps.

## Configure

This is the shape the replacement integration is expected to keep — a host-resolved endpoint plus one
stored secret, with the integration itself never seeing either.

Set the two non-secret values and store the one secret. Zendesk API-token Basic auth requires the
literal `/token` suffix on the username:

```bash
export ZENDESK_URL="https://company.zendesk.com"
export ZENDESK_USER="agent@example.com/token"
flux auth set zendesk                 # hidden prompt; purpose is inferred as api_token
flux plugin status zendesk
flux plugin call zendesk zendesk.test '{}'
```

`ZENDESK_API_TOKEN` is an environment fallback if a stored credential is not desired. The plugin
receives neither the endpoint URL nor the token: it names `zendesk.endpoint` and auth purpose
`api_token`; flux resolves both, injects Basic auth, applies the network guard, and redacts secrets.

## Run one entrypoint

```bash
flux run examples/zendesk.triage.flux --entry setup --yes
flux run examples/zendesk.triage.flux --entry triage \
  --arg 'query=type:ticket status:new' --yes
flux run examples/zendesk.triage.flux --entry brief --arg ticket_id=12345 --yes
flux run examples/zendesk.triage.flux --entry eod \
  --inputs '{"query":"type:ticket updated>24hours"}' --yes
```

`setup` only verifies auth. `triage` ranks one result page, `brief` fetches a ticket and one comment
page concurrently, and `eod` summarizes one bounded search page. Search and comment reads accept at
most 100 records per call; pagination links are returned but this example does not auto-page.

The setup entrypoint needs no model. The other three call `ai.extract`, so configure a normal Flux
model/provider. Their cognition blocks have timeouts and deterministic fallback: if the model is
unavailable, the returned value still contains the API evidence gathered before it.

## Data exposure and write boundary

Running `triage`, `brief`, or `eod` intentionally sends bounded Zendesk ticket fields—and, for
`brief`, internal comment bodies—to the configured model provider. Apply your organization’s data
handling rules and choose the provider accordingly. The model never authors Flux and cannot choose
an operation name or mutation payload in this workflow.

The reference file contains no Zendesk write operation. Operators can separately invoke the narrow
write API, each through normal approval/policy gating:

```bash
flux plugin call zendesk zendesk.ticket.update \
  '{"ticket_id":12345,"updated_stamp":"2026-07-30T12:00:00Z","priority":"high"}'

flux plugin call zendesk zendesk.ticket.comment.add \
  '{"ticket_id":12345,"updated_stamp":"2026-07-30T12:00:00Z","body":"Investigating"}'

flux plugin call zendesk zendesk.ticket.tag.add \
  '{"ticket_id":12345,"updated_stamp":"2026-07-30T12:00:00Z","tags":["triaged"]}'
```

All three set Zendesk `safe_update=true` and require the last observed `updated_stamp`; a stale
write fails for refetch instead of overwriting newer work. Comments default to an internal note.
Only explicit `"public": true` sends a public reply. Tag addition uses `additional_tags`, so it does
not replace existing tags.
