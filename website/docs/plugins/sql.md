---
title: SQL plugin
description: "Step-by-step setup for the sql plugin: install, configure a DSN, grant private-network egress, verify, and run bounded read-only queries."
---

# SQL plugin

A worked setup for the `sql` plugin — read-only query and schema introspection against a PostgreSQL
or MySQL/MariaDB database. This page walks through the exact sequence using only the `flux` CLI. For
the general plugin mechanics (capability grants, trust model, everyday commands), see
[Using plugins](./using-plugins.md).

`sql` is the strictest plugin in the pack, in two directions at once. It **never receives the
password**: the host speaks the database's startup and authentication handshake itself and hands the
plugin a post-authentication connection. And it **cannot write**: every query passes a read-only
statement whitelist before a socket is dialed.

## 1. Install

```bash
flux plugin install sql
```

This resolves the newest signed `plugins-v*` pack release, verifies the index signature and the
archive's sha256, and unpacks the binary into the versioned store. Confirm it landed:

```bash
flux plugin status sql
```

```text
sql              ~/.flux/plugins/bin/sql/0.1.2/flux-plugin-sql   v0.1.2  [ok]  [verified]
    manifest:  v0.1.2  6 op(s)  ·  1 auth purpose(s)  ·  1 endpoint(s)  ·  1 datasource(s)  ·  caps: conn(2)
    auth:      · password — not configured (env: SQL_PASSWORD, MYSQL_PASSWORD, or `flux auth set sql password`)
    endpoint:  · sql.endpoint — not configured (env: SQL_DSN, SQL_URL)
```

`caps: conn(2)` is the whole capability set: raw TCP to port 5432 and port 3306, and nothing else.
There is no `http`, no `process`, and — notably — no `secret(…)`. The plugin holds a credential
*location*, never a value.

## 2. Configure the endpoint

Two environment variables, both read by the **host**, never by the plugin:

| What | Env vars (first one set wins) | Read by |
|---|---|---|
| Connection DSN | `SQL_DSN`, `SQL_URL` | the host, to resolve the `sql.endpoint` reference and to hand the plugin non-secret metadata |
| Password | `SQL_PASSWORD`, `MYSQL_PASSWORD` | the host only, for the handshake it terminates |

```bash
export SQL_DSN="postgres://app@db.example.com:5432/warehouse"
export SQL_PASSWORD="…"
```

:::caution
**Keep the password out of the DSN.** The DSN doubles as a declared *non-secret* config value the
plugin reads for connection metadata (dialect, database, username), and the host refuses
credential-bearing values on that path. A DSN with an inline password will not resolve.
:::

Re-run `flux plugin status sql` and both lines flip to `✓`:

```text
    auth:      ✓ password — env $SQL_PASSWORD
    endpoint:  ✓ sql.endpoint — postgres://app@db.example.com:5432/warehouse (env $SQL_DSN)
```

The status line also offers `flux auth set sql password` as an alternative. On this static-endpoint
path the host resolves the handshake credential from the **declared environment keys only**, so use
`SQL_PASSWORD` here; stored tokens apply to the plugins that authenticate over HTTP.

The plugin never dials a URL it parsed. It asks the host to dial `sql.endpoint` *by name*; the host
resolves the reference, applies the egress guard, opens the socket, speaks the PostgreSQL
StartupMessage / SCRAM-SHA-256 (or MySQL Handshake v10) exchange, and returns a connection that is
already authenticated. See [Plugin capability sandbox](../security/plugin-sandbox.md) for why this
path exists and what it replaced.

## 3. Database on a private network? Grant egress

A database on an internal or loopback address is refused by the SSRF guard by default — you'll see
this if you skip this step:

```text
error: plugin `sql` op `sql.test`: refusing to fetch private/loopback/link-local address 10.1.2.3 …
```

Grant the specific host in `.flux/config.toml` (project) or `~/.flux/config.toml` (user default):

```toml
[private_net.plugins]
sql = ["db.internal.example", "127.0.0.1"]
```

The grant is intersected with what the plugin declares, and every admitted private-address call is
audited. A managed database on a public address needs no grant. See
[Private-network egress](../reference/config.md#private-network-egress) for the full mechanism.

## 4. Verify

```bash
flux plugin call sql sql.test
```

`sql.test` opens a connection and runs `SELECT 1` — the cheapest end-to-end check that the DSN,
password, egress grant, and server version all line up:

```json
{
  "status": "ok",
  "endpoint_url": "postgres://app@db.example.com:5432/warehouse",
  "driver": "postgres",
  "database": "warehouse",
  "server_version": "16.2"
}
```

`endpoint_url` is always password-redacted. A missing DSN fails with
``endpoint has no DSN configured (set SQL_DSN or SQL_URL)``; a wrong password surfaces as the
server's own authentication error, which tells you the wiring is fine and the credential is not.

## 5. Query and introspect

Six operations, all read-only. Every one accepts the shared connection fields
`{endpoint?, endpoint_ref?, driver?, database?, timeout?}` in addition to its own.

| Operation | Own arguments | Returns |
|---|---|---|
| `sql.test` | — | `status`, `driver`, `database`, `server_version` |
| `sql.query` | `query`, `max_rows?` | `columns[]`, `rows[]`, `row_count`, `truncated` |
| `sql.database.list` | — | `databases[]` with `name`, `kind`, `current` |
| `sql.table.list` | `schema?`, `include_views?`, `max_results?` | tables with a cheap row estimate |
| `sql.table.show` | `table`, `schema?` | `columns[]`, `primary_key[]`, `foreign_keys[]` |
| `sql.index.list` | `table?`, `schema?` | `indexes[]` with `columns`, `unique`, `primary` |

```bash
flux plugin call sql sql.database.list
flux plugin call sql sql.table.list --arg schema=public --arg include_views=true
flux plugin call sql sql.table.show --arg table=orders
flux plugin call sql sql.index.list --arg table=users
flux plugin call sql sql.query '{"query": "SELECT id, email FROM users ORDER BY id", "max_rows": 50}'
```

```json
{
  "columns": ["id", "email"],
  "rows": [{"id": "1", "email": "ada@example.com"}, {"id": "2", "email": null}],
  "row_count": 2,
  "truncated": false
}
```

`max_rows` defaults to 100 and is capped at 1000; `truncated` tells you when the cap bit. Result rows
are also contributed to the `sql.query_rows` datasource, so an agent can search them later.

**The read-only guard.** `sql.query` accepts a single statement beginning `SELECT`, `SHOW`,
`DESCRIBE`, `EXPLAIN`, or `WITH`, and rejects anything containing a write keyword outside a function
call. Multi-statement input, write CTEs, and `INTO OUTFILE` / `DUMPFILE` are refused. The check runs
**before any connection is dialed**:

```text
SQL query must be read-only; allowed statements are SELECT, SHOW, DESCRIBE, EXPLAIN, and WITH
```

This is the plugin's own guard, on top of whatever the database grants the connecting role. Point it
at a read-only role anyway.

## 6. Dialects

- **PostgreSQL** — the primary target. All six operations run whitelisted introspection SQL.
- **MySQL / MariaDB** — supported, with per-dialect introspection SQL. Note that
  `sql.database.list` means something different here: MySQL treats schema and database as one
  object, so every entry is `kind: "database"` and no `kind: "schema"` entries are returned, where
  PostgreSQL returns both levels. Authentication is `mysql_native_password`;
  `caching_sha2_password` (the MySQL 8.0+ default), `ed25519`, and `parsec` are not implemented and
  fail with an error naming the workaround.
- **SQLite** — unsupported by design. SQLite is a local file, and this plugin's only IO capability is
  a socket.

## 7. Discovered endpoints, no configuration

The DSN above is the static path. The other path is a **discovered** endpoint: an agent asks
[`endpoint.discover`](../agent/endpoints.md#agent-operations) for a `postgres` product, a provider
plugin such as [`kubernetes`](./kubernetes.md) returns a credential-free weak reference, and
`endpoint.select` hands the whole reference object to `sql.query`:

```flux
flow inspect-database(endpoint_id: String)
  $endpoint = endpoint.select($endpoint_id)
  $rows = sql.query({endpoint: $endpoint, query: "SELECT current_database() AS name", max_rows: 1})
  return $rows
```

Pass the reference as the `endpoint` **object**. A bare `@endpoint/<id>` string in `endpoint_ref` is
rejected with an explicit error — `endpoint_ref` names a static manifest endpoint, and the id-only
lookup is retired.

When the discovered credential is owned by a different plugin, resolution is deny-by-default and
needs an operator grant:

```toml
[endpoint]
cross_plugin_credentials = ["sql:kubernetes"]
```

First use still crosses approval and produces an audit event. The plugin receives the reference's
`credential_ref` — a location such as `kubernetes/team/db-creds/password` — and passes it back to the
host, which resolves it for the handshake it terminates. The value never enters the plugin.

## Recap

| Step | Command | Failure mode if skipped |
|---|---|---|
| Install | `flux plugin install sql` | ``no such plugin `sql` `` |
| DSN + password | `export SQL_DSN=… SQL_PASSWORD=…` | `endpoint has no DSN configured (set SQL_DSN or SQL_URL)` |
| Private-net grant (internal DB only) | `[private_net.plugins]` in config | `refusing to fetch private/loopback/link-local address …` |
| Verify | `flux plugin call sql sql.test` | (this *is* the verification step) |
| Cross-plugin grant (discovered endpoints) | `[endpoint] cross_plugin_credentials` | the discovered credential is refused to `sql` |

## Related docs

- [Using plugins](./using-plugins.md) — install, pin, capability grants, and the trust model shared
  by every plugin.
- [Endpoints](../agent/endpoints.md) — weak references, discovery, and the operator CLI that owns
  the worked example this page's discovered path continues.
- [Kubernetes plugin](./kubernetes.md) — the provider that discovers in-cluster database endpoints.
- [Plugin capability sandbox](../security/plugin-sandbox.md) — `conn.authenticate` and the
  references-only IO model.
- [Configuration](../reference/config.md) — `[private_net.plugins]` and
  `[endpoint] cross_plugin_credentials`.
