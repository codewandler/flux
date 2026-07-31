# Zendesk triage workflow

> **The backing integration has been replaced, and the workflow still cannot reach a live account.**
> The `flux-plugin-zendesk` plugin was **removed before its first release** — no signed pack was ever
> published, so nothing was withdrawn from users. Its replacement is flux-connectors'
> `connector-pack`, which now serves these operations under the **same names this flow already
> called**. What remains are two connector-side gaps, named in
> [Reaching a live account](#reaching-a-live-account) below; neither is a missing credential.

The reference flow is [`examples/zendesk.triage.flux`](../examples/zendesk.triage.flux). It
demonstrates a deterministic third-party workflow: Flux-Lang fixes the control graph and literal
operation names, the integration performs typed API calls through guarded egress, and AI is confined
to bounded analysis steps.

## Where the operations come from

There is no plugin to install and no `flux plugin call` path. A **host** registers the connector Tool
pack when it builds its client, and each catalogue operation arrives as one ordinary dotted tool:

```rust
// flux's own `http.request`, already configured with this host's SSRF guard and audit sink.
let http = Egress::new(Arc::new(flux_web::http::HttpRequestTool::new(&web_options)));
let credentials = Credentials::new(host_secret_store, &tenant)?;

let client = flux_sdk::Client::builder()
    .try_register_pack(connector_pack::pack(&["zendesk"], http, credentials))
    .build()?;
```

Two properties are worth stating because they are the reason this shape was chosen over more
composite Flux:

- **flux keeps every byte of egress.** Each generated tool builds `{ method, url, headers, body }`
  and hands it to flux's own `http.request` under the *same* `ToolContext`. The pack opens no socket,
  holds no HTTP client, and resolves no hostname.
- **Each operation is gated individually**, at the risk level the connector author declared, by this
  host's permission and approval envelope — not by whatever gating `http.request` happens to receive.
  Because the inner delegation calls `http.request::execute` directly and so bypasses
  `Executor::dispatch`, each generated tool mirrors `http.request`'s own `permission_subjects` and
  `NetworkFetch` intent itself; the pack's `tests/network_gate.rs` holds every shipped operation to
  that. If it did not, installing a connector would be a hole through this host's network policy.

The operation names did not have to move. The pack projects the catalogue id `zendesk-test` to
`zendesk.test` and `zendesk-ticket-comment-list` to `zendesk.ticket.comment.list`, which is what this
flow already called — the pack was authored to this shape. The set is pinned from both ends: by
`flux-cli`'s `zendesk_reference_calls_exactly_the_connector_pack_read_operations` here, and by the
pack's `tests/projection.rs` there.

## Reaching a live account

Both remaining blockers are in flux-connectors, and **both refuse rather than sending a broken
request** — which is why neither shows up as a confusing vendor `401`:

1. **No credential address.** `providers/zendesk.toml` declares no `authority`, so there is no
   `tenants/<tenant>/<authority>/<credential>` path to resolve and the pack answers
   `NoCredentialAddress`. Only 7 of the shipped connectors declare an authority today
   (flux-connectors C-37). Storing a token does not work around this: the address, not the value, is
   what is missing.
2. **No config resolution.** Zendesk's `base_url` is `https://{subdomain}.zendesk.com` — every
   account lives on its own subdomain, so there is no tenant-independent URL. The pack does not yet
   resolve the `[[config]]` field that binds `{subdomain}`, so a built request URL carries the
   placeholder verbatim and names a host that does not resolve (flux-connectors C-86/C-68). This
   affects 27 of 105 shipped operations, not just Zendesk.

Until both close, every entrypoint fails at its first call. The difference from the plugin's
withdrawal is that it now fails in a named, fixable place rather than at an operation nothing serves.

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

The offline proof of all four is `crates/flux-eval/tests/zendesk_triage.rs`, which lowers the
checked-in module and executes every entrypoint against static fixtures — no credential, provider,
or network. It keeps passing while the gaps above are open, and it is what keeps this shape honest.

## Data exposure and write boundary

Running `triage`, `brief`, or `eod` intentionally sends bounded Zendesk ticket fields — and, for
`brief`, internal comment bodies — to the configured model provider. Apply your organization's data
handling rules and choose the provider accordingly. The model never authors Flux and cannot choose an
operation name or mutation payload in this workflow.

**What read-only means here has changed shape, and the difference matters.** `pack(&["zendesk"])`
registers all seven catalogue operations, so `zendesk.ticket.update`, `zendesk.ticket.comment.add`
and `zendesk.ticket.tag.add` *are* present in the host's registry — the plugin era's separate
`flux plugin call` surface is gone. What is guaranteed is narrower and is asserted rather than
asserted-in-prose: no write is reachable from any of the four entrypoints, checked against this
module's own call graph. A host that wants the writes unreachable at all withholds approval for them,
which is where that decision belongs.

The write-safety declarations survived the migration into the connector catalogue:

- `zendesk-ticket-update` requires `updated_stamp` and carries `safe_update` as a **constant `true`**
  the caller cannot supply or drop — that constant is what makes the stamp binding, and without it
  every write becomes a last-write-wins race. Its idempotency is declared `conditional`: the same
  call replayed after the ticket moved is rejected by Zendesk rather than applied.
- The response is declared as `{ ticket, audit }`, not the bare ticket a read returns. `audit.events`
  is the diagnostic that matters: a flat body Zendesk accepts, ignores and answers `200` to is
  indistinguishable from a real update by status code alone, and visible here and nowhere else.
- A comment is an internal note unless `public` is explicitly `true`; tag addition is additive rather
  than replacing.
