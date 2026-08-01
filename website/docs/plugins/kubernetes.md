---
title: Kubernetes plugin
description: "Step-by-step setup for the Kubernetes plugin: install, point kubectl at a cluster, verify, read inventory, and use guarded mutations."
---

# Kubernetes plugin

A worked setup for the `kubernetes` plugin — cluster contexts, read-only inventory (namespaces,
services, pods, deployments, containers, ingresses, nodes), debugging (logs, events, exec, rollout
history), managed port-forwards, and a small set of guarded mutations. This page walks through the
exact sequence using only the `flux` CLI. For the general plugin mechanics (capability grants, trust
model, everyday commands), see [Using plugins](./using-plugins.md).

The plugin drives the **`kubectl` CLI** through the host's process capability. It speaks no
Kubernetes API of its own and declares no auth purpose: your kubeconfig is what authenticates, and
`kubectl` must be on `PATH`.

## 1. Install

```bash
flux plugin install kubernetes
```

This resolves the newest signed `plugins-v*` pack release, verifies the index signature and the
archive's SHA-256, and unpacks the binary into the versioned store. Confirm it landed:

```bash
flux plugin status kubernetes
```

`status` reads the installed version and operation total from the live manifest. For setup, check
the `kubernetes.inventory` datasource, discovery for `kubernetes`, `prometheus`, `loki`, `grafana`,
`alertmanager`, `postgres`, and `mysql`, and the eight process prefixes listed below.

Unlike the HTTP integrations there are no `auth:` or `endpoint:` lines — the manifest declares
neither. What it *does* declare is `process(8)`: eight argv prefixes, and nothing else.

```text
kubectl get           kubectl logs      kubectl config view   kubectl version
kubectl exec          kubectl scale     kubectl rollout restart   kubectl port-forward
```

Process grants are argv **prefixes**, matched token by token. `kubectl get` authorizes
`kubectl get pods -o json`; it does not authorize `kubectl delete`, `kubectl apply`, or
`kubectl patch` — those verbs are absent from the manifest, so this plugin is *structurally* unable
to issue them even against a kubeconfig with cluster-admin. Each operation narrows further to its
own verbs, and that narrowing is what approval prompts and audit records disclose.

## 2. Point kubectl at a cluster

There is no plugin-side configuration step. The plugin runs `kubectl`, and `kubectl` finds its
config the way it always does: `KUBECONFIG` if set, otherwise `~/.kube/config`.

```bash
kubectl config get-contexts        # what you already have
```

Flux spawns the plugin through the guarded process path, which clears the environment and re-adds
only a minimal non-secret allow-list. `KUBECONFIG` is on that list, alongside `PATH` and `HOME`, so
a kubeconfig at a non-default path resolves for the plugin exactly as it does in your own shell.
What is forwarded is the *path* — the plugin opens the file itself, through `kubectl`. The rest of
the allow-list is the same fixed non-secret set every plugin subprocess gets — locale, `TMPDIR`,
your username, toolchain locations, logging knobs and the like. Nothing outside that list survives
the clear, so a cloud token, an API key or any other export sitting in your shell never reaches the
plugin.

Every operation takes an optional `context` naming a kubeconfig context; omitting it uses the
current one. The cluster inventory is available without any further grant — the plugin reaches the
cluster through `kubectl`, not through Flux's HTTP stack, so a private API server needs no
`[private_net.plugins]` entry.

## 3. Verify

```bash
flux plugin call kubernetes kubernetes.cluster.list
flux plugin call kubernetes kubernetes.test
```

`kubernetes.cluster.list` reshapes `kubectl config view` into the contexts you can target;
`kubernetes.test` probes reachability by running `kubectl version` against one of them — the
cheapest end-to-end check that the kubeconfig, the network, and your credentials all line up:

```json
{ "context": "prod", "ok": true, "server_version": "v1.30.4", "platform": "linux/amd64" }
```

If `kubectl` starts but exits non-zero, `kubernetes.test` returns `{"ok": false, "error": "…"}`
with its captured stderr. If Flux cannot spawn `kubectl` at all — for example because it is absent
from `PATH` — the plugin call itself fails with a `process.run` spawn error; there is no `ok: false`
result in that case. Other operations such as `kubernetes.cluster.list` also surface non-zero
`kubectl` exits as operation errors because they require valid JSON output.

## 4. Read the cluster

The inventory operations are read-only and share one input shape —
`{context?, namespace?, query?, limit?}`. Omitting `namespace` lists across all namespaces; `query`
is a case-insensitive substring filter applied to the whole object; `limit` truncates.

```bash
flux plugin call kubernetes kubernetes.namespace.list
flux plugin call kubernetes kubernetes.pod.list --arg namespace=prod --arg limit=20
flux plugin call kubernetes kubernetes.service.list '{"namespace": "prod", "query": "api"}'
flux plugin call kubernetes kubernetes.deployment.list --arg namespace=prod
flux plugin call kubernetes kubernetes.ingress.list
flux plugin call kubernetes kubernetes.container.list --arg namespace=prod
flux plugin call kubernetes kubernetes.node.list --arg limit=10
```

These return `kubectl -o json` shape — an `items` array of real Kubernetes objects, filtered and
truncated but not reshaped — so anything you know about a manifest is still true of the result. The
`*.show` variants (`kubernetes.service.show`, `kubernetes.pod.show`, `kubernetes.deployment.show`,
`kubernetes.container.show`) take `{context?, namespace?, name}` and return the single object.

Each list operation also contributes `kubernetes.<kind>` records to the `kubernetes.inventory`
datasource, so an agent can search live cluster state through the ordinary
[datasource](../agent/datasources.md) surface instead of re-listing.

## 5. Debug a workload

```bash
flux plugin call kubernetes kubernetes.pod.logs \
  '{"namespace": "prod", "name": "api-7d9f-abcde", "tail_lines": 200, "since": "15m"}'
flux plugin call kubernetes kubernetes.pod.logs \
  '{"namespace": "prod", "selector": "app=api", "container": "server"}'
flux plugin call kubernetes kubernetes.event.list \
  '{"namespace": "prod", "warnings_only": true, "limit": 20}'
flux plugin call kubernetes kubernetes.deployment.history --arg namespace=prod --arg name=api
```

`kubernetes.pod.logs` needs `namespace` plus either `name` or `selector`, and returns
`{namespace, name, selector, container, line_count, lines[]}` — bounded by `tail_lines` (default
100) and optionally `limit_bytes`. `until` accepts an RFC3339 upper bound and is applied
client-side. `kubernetes.deployment.history` lists a deployment's ReplicaSet revisions, newest
first, with images and replica counts.

The following operations carry a higher declared risk and go through approval when an agent calls
them:

| Operation | Input | Declared |
|---|---|---|
| `kubernetes.pod.exec` | `{namespace, name, command[], container?, timeout_seconds?}` | `[Process, Network]`, high risk, non-idempotent |
| `kubernetes.secret.read` | `{namespace, name, keys?}` | `[Read, Network]`, high risk — returns decoded secret material |
| `kubernetes.deployment.scale` / `.restart` | `{namespace, name, replicas}` / `{namespace, name}` | `[Write, Network]`, high risk |

`kubernetes.pod.exec` is one-shot: no TTY, no stdin. It returns bounded stdout/stderr with the exit
code. `kubernetes.secret.read` decodes a Secret's values and is meant for piping into an auth or
secret store, not for display — prefer endpoint discovery (step 7 below), which returns a credential
*location* and never reads the value at all.

## 6. Manage a port-forward in a live session

Port-forwards are held by the host's managed-process registry, so they outlive the operation call
that started them **within the same live Flux session**. They are intended for a long-running agent
or app session, not a sequence of separate one-shot CLI processes.

```bash
# Validate the input shape without starting a process:
flux plugin call kubernetes kubernetes.portforward.start \
  '{"namespace": "monitoring", "resource": "service/prometheus", "remote_port": 9090}' --dry-run
```

When an agent invokes that operation in a live session, the result has this shape:

```json
{
  "id": "kpf-4242", "started": true, "namespace": "monitoring",
  "resource": "service/prometheus", "address": "127.0.0.1",
  "local_port": 19090, "remote_port": 9090,
  "local_url": "http://127.0.0.1:19090",
  "duration_seconds": 3600, "expires_at": "…"
}
```

Omit `local_port` and kubectl picks a free one, recovered from its readiness line. A spawn failure is
an immediate `process.spawn` operation error. If `kubectl` starts but exits before its readiness
line, the operation fails with `kubectl port-forward … did not become ready` plus captured output.
A returned `id` therefore means the forward is up. `duration_seconds` defaults to one hour and is
capped at eight hours.

In that same live session, `kubernetes.portforward.list` reports the forward's liveness and
`kubernetes.portforward.stop` accepts its `id`. Separate `flux plugin call` invocations each start a
fresh plugin host; dropping that host also drops and kills its managed child, so those invocations do
not form a persistent start/list/stop sequence.

`list` probes each forward for liveness and reports `alive`. It only knows about forwards **this**
plugin instance started — it is the plugin's own view, not a query of every managed process on the
host.

## 7. Discover endpoints for other plugins

`kubernetes` is the pack's main endpoint **provider**. It advertises discovery for `kubernetes`,
`prometheus`, `loki`, `grafana`, `alertmanager`, `postgres`, and `mysql`, so an agent asking
"what Postgres is in this cluster?" is routed here:

```bash
flux plugin call kubernetes kubernetes.endpoint.discover --arg product=prometheus --arg namespace=monitoring
flux plugin call kubernetes kubernetes.endpoint.discover --arg product=postgres --arg cluster=dev
```

Candidates come back as weak references — id, credential-free URL, product, labels, a score, and the
reasons it matched:

```json
{"candidates": [
  { "id": "@endpoint/monitoring-prometheus", "product": "prometheus", "protocol": "http",
    "source": "discovered", "url": "http://prometheus.monitoring.svc.cluster.local:9090",
    "labels": {"namespace": "monitoring", "service": "prometheus"}, "score": 1.0,
    "reasons": ["service name `prometheus` matches `prometheus`"] }
]}
```

`product=kubernetes` yields one endpoint per kubeconfig context. Other products match in-cluster
Services and Ingresses by name or `app.kubernetes.io/name`. `postgres` and `mysql` additionally scan
Secrets for a connection pattern (a host/endpoint key plus a password-like key) and return a
`credential_ref` of the form `kubernetes/<namespace>/<secret>/<key>` — a **location**, never the
value. `cluster` is a short alias (`dev`) resolved against context names; an ambiguous alias is a
loud error rather than a silent empty result.

Handing one of those references to a consumer such as the [SQL plugin](./sql.md) crosses a
plugin boundary, which is deny-by-default and needs an operator grant:

```toml
[endpoint]
cross_plugin_credentials = ["sql:kubernetes"]
```

First use still crosses approval and is audited. See [Endpoints](../agent/endpoints.md) for the
`endpoint.discover` / `endpoint.select` / `endpoint.import` operations an agent uses on top of this.

## Recap

| Step | Command | Failure mode if skipped |
|---|---|---|
| Install | `flux plugin install kubernetes` | ``no such plugin `kubernetes` `` |
| kubectl + `~/.kube/config` | `kubectl config get-contexts` | Missing `kubectl` → spawn error; bad context/auth/network after spawn → `kubernetes.test` returns `"ok": false`. |
| Verify | `flux plugin call kubernetes kubernetes.test` | (this *is* the verification step) |
| Cross-plugin credential grant | `[endpoint] cross_plugin_credentials` | a discovered credential is refused to the consumer plugin |

## Related docs

- [Using plugins](./using-plugins.md) — install, pin, capability grants, and the trust model shared
  by every plugin.
- [SQL plugin](./sql.md) — the usual consumer of a `postgres` endpoint discovered here.
- [Endpoints](../agent/endpoints.md) — weak references, discovery, and the operator CLI.
- [Plugin capability sandbox](../security/plugin-sandbox.md) — argv-prefix process grants and the
  rest of the manifest gates.
- [Configuration](../reference/config.md) — `[endpoint] cross_plugin_credentials` and private-network
  grants.
