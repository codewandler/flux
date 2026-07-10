---
title: Endpoints
description: "Discover, inspect, persist, and consume live service endpoints as weak references without exposing credential values."
---

# Endpoints

An endpoint is a model-safe reference to a live service: where it is, what protocol it speaks, and
the **location** of any credential. It never contains the credential value. Use endpoints for live
systems such as PostgreSQL, Prometheus, or an in-cluster API.

Endpoints and [datasources](./datasources.md) solve different problems:

- A datasource is an indexed knowledge store read through `sources`, `search`, and `get`.
- An endpoint is a connection target passed to an operation such as `sql.query`.

Connecting a database does not automatically index its rows as knowledge.

## The weak-reference boundary

An `EndpointRef` contains an id, credential-free URL, product/protocol hints, non-secret labels, and
an optional `credential_ref` such as `env/POSTGRES_PASSWORD` or
`kubernetes/team/db-creds/password`. The host resolves that location only when an operation connects.
The resolved URL/auth form is host-only and cannot be serialized into model context.

Cross-plugin credentials are deny-by-default. For example, allowing the `sql` plugin to use a
credential owned by `kubernetes` requires:

```toml
[endpoint]
cross_plugin_credentials = ["sql:kubernetes"]
```

First use still crosses approval and produces an audit event.

## Agent operations

| Operation | Purpose |
|---|---|
| `endpoint.discover({product, query?, limit?})` | Ask installed provider plugins for ranked weak references. |
| `endpoint.list()` | List records currently known to the session. |
| `endpoint.info(id)` | Inspect one record, including the credential location—not value. |
| `endpoint.select(id)` | Return one `EndpointRef` value to pass to another operation. |
| `endpoint.import(id)` | Persist a known record to `~/.flux/endpoints.toml`; this is a local write and may prompt. |

The group surfaces when a kubeconfig is available or the persisted endpoint store was non-empty at
session startup. Discovery also needs an installed provider plugin that advertises the requested
product. A newly persisted endpoint is picked up automatically by later sessions.

```flux
flow inspect-database(endpoint_id: String)
  $endpoint = endpoint.select($endpoint_id)
  $rows = sql.query({endpoint: $endpoint, query: "SELECT current_database() AS name", max_rows: 1})
  return $rows
```

The `sql` plugin performs read-only PostgreSQL queries. For discovered credentials, the host can
complete PostgreSQL authentication and hand the plugin an authenticated connection without sending
the password through the plugin protocol.

## Operator CLI

The CLI mirrors the persisted store and never resolves a secret value:

```bash
flux endpoint add pg-prod \
  --url postgres://db.example:5432/app \
  --product postgres \
  --protocol postgres \
  --credential-ref env/POSTGRES_PASSWORD \
  --label environment=production
flux endpoint list
flux endpoint show pg-prod
flux endpoint resolve pg-prod
flux endpoint import @endpoint/orders-db
```

- `add` validates and persists a named, config-bound weak reference to
  `~/.flux/endpoints.toml`. Named ids must not start with `@endpoint/`, which is reserved for
  discovered services.
- `list` and `show` display weak-reference metadata.
- `resolve` explains what source, bare URL, and credential-reference location would be used at
  connect time.
- `import` persists a discovered record already known in a session. From a standalone shell,
  provide a complete weak reference with `--from-json` when it is not already in the file.

```bash
flux endpoint import @endpoint/local-postgres --from-json \
  '{"id":"@endpoint/local-postgres","url":"postgres://127.0.0.1:5432/app","product":"postgres","protocol":"postgres","source":"config","credential_ref":{"scheme":"env","plugin":"","instance":"","slot":"POSTGRES_PASSWORD"},"labels":{"environment":"local"}}'
```

Neither `add` nor `import` tests reachability. Inline credentials in the URL are forbidden; use a
credential reference instead. Re-running `add` with the same id replaces that weak reference.

For config-as-code, declare the same binding in user or project configuration:

```toml
[[endpoint.static]]
id = "pg-prod"
url = "postgres://db.example:5432/app"
product = "postgres"
protocol = "postgres"
credential_ref = "env/POSTGRES_PASSWORD"
labels = { environment = "production" }
```

Project entries override user entries with the same id. Static declarations are merged into the
session registry at startup and resolve like records created with `endpoint add`; they are not
written back to `~/.flux/endpoints.toml` merely by loading the config.

## Related docs

- [Datasources](./datasources.md) — indexed knowledge, distinct from connection targets.
- [Using plugins](../plugins/using-plugins.md) — install `kubernetes`, `sql`, and other providers/consumers.
- [Configuration](../reference/config.md) — cross-plugin credentials and private-network grants.
- [Plugin capability sandbox](../security/plugin-sandbox.md) — host-side reference resolution.
