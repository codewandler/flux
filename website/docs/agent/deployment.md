---
title: Deploy the agent to Kubernetes
description: "Run flux's agent surface in a cluster with the released image, reach it from your workstation with flux a2a, and know what a restart keeps."
---

# Deploy the agent to Kubernetes

This page deploys **the agent itself** into a cluster and reaches it from your laptop. The agent
plans, calls the model, runs tools and keeps its sessions in the pod; what you run locally is a thin
client.

That is one of two things "remote" can mean, and picking the wrong one costs an afternoon:

|  | **Remote agent** — this page | **Remote substrate** — [remote-system deployment](../remote-system-deployment.md) |
|---|---|---|
| the pod runs | `flux app run --serve` | `flux system serve` |
| what moves | the whole agent: planning, model calls, session | only where guarded effects land |
| what stays local | a thin client | the runtime, model, credentials, approvals, session store |
| the pod needs a model credential | **yes** | no |
| you reach it with | `flux a2a <url>` | `--host <name>` / `--remote <url>` |
| you want it when | the agent should outlive your terminal, or somebody else's machine should own the loop | you want to approve here and have it happen there |

**Flux does not provision the cluster.** Nothing here creates a cluster, a node pool, a storage
class or an ingress controller — no more than the substrate profile creates Firecracker microVMs or
Kata guests. The manifests configure infrastructure you already have.

## What you apply

The shipped Kustomize base is [`deploy/agent/`](https://github.com/codewandler/flux/tree/main/deploy/agent).
It runs the **same released image** as the substrate profile, selecting the agent surface with a
command override rather than a second image:

```yaml
command: [/usr/local/bin/flux, app, run]
args: [--no-sandbox, --store, /srv/flux/state, --serve=0.0.0.0:8787, --yes]
```

Build that image from a release exactly as before — `deploy/container/build-image.sh --release
<version>` — and push it to a registry your cluster can pull from. The provenance path
(`gh attestation verify` against the published archive) covers this deployment unchanged, because
it is the same bytes.

### 1. Create the two Secrets

Neither is in the base, because a checked-in manifest is where credentials must never be.

```sh
kubectl create namespace flux-agent

# The bearer token every caller presents.
kubectl -n flux-agent create secret generic flux-agent-token \
  --from-file=token=/dev/stdin <<<"$(openssl rand -base64 39)"

# The credential the agent's own model calls use.
kubectl -n flux-agent create secret generic flux-agent-model-credentials \
  --from-file=api-key=/dev/stdin <<<"$ANTHROPIC_API_KEY"
```

The manifest names `ANTHROPIC_API_KEY`, which flux's default `sonnet` alias resolves against. For a
different provider, rename that env var and add `--model <spec>` to the container's arguments.

### 2. Apply

```sh
kubectl kustomize deploy/agent | kubectl apply --dry-run=client -f -   # check first
kubectl apply -k deploy/agent
kubectl -n flux-agent rollout status deployment/flux-agent
```

## Reaching it from your workstation

The Service is ClusterIP, so a port-forward is the zero-config route in:

```sh
kubectl -n flux-agent port-forward service/flux-agent 8787:8787
```

Confirm the agent answered before involving a client. The agent card is a discovery endpoint and
needs no token:

```sh
curl -s http://127.0.0.1:8787/.well-known/agent-card.json | jq .name
```

Then talk to it. `flux a2a` reads its bearer token from `FLUX_A2A_TOKEN`:

```sh
export FLUX_A2A_TOKEN=$(kubectl -n flux-agent get secret flux-agent-token \
  -o jsonpath='{.data.token}' | base64 -d)

flux a2a http://127.0.0.1:8787                     # interactive REPL
flux a2a http://127.0.0.1:8787 "summarize the repo" # one turn, then exit
```

Everything else on that port is authenticated: `POST /a2a` (`message/send`, `message/stream`),
`POST /sessions`, `GET /sessions/{id}`, `POST /sessions/{id}/messages`. Only `/health` and the agent
card are exempt, which is what lets the pod's probes work without a token in the manifest.

### The listener is plain HTTP

`flux app run --serve` terminates no TLS. The bearer token authenticates the caller to the agent and
does nothing in the other direction, so anyone who can read the connection can replay it. A
port-forward is a local tunnel over the API server's own TLS, which is fine. If the endpoint leaves
the cluster, terminate TLS at an ingress and keep the hop from there to the pod inside a boundary
you trust.

## Authentication is not optional here

A non-loopback agent listener must be authenticated. This is a release boundary, not a
recommendation, and it is enforced twice over so neither half depends on the other being right:

- The manifest populates `FLUX_SERVER_TOKEN` from a required Secret. A namespace missing that Secret
  gets a container that never starts.
- The binary refuses the bind regardless of what any manifest says:

  ```
  error: refusing to serve on a non-loopback address (0.0.0.0:8787) without authentication — set
  FLUX_SERVER_TOKEN to require `Authorization: Bearer <token>` (or configure `[server]
  introspect_url` for per-request principal auth), or bind 127.0.0.1
  ```

There is no flag that turns this off. An open, self-approving agent on a cluster network is remote
code execution.

## Choosing the approval posture

`flux app run --serve` with no program refuses to start until you choose one, because an HTTP
request has no terminal to prompt at:

- **`--yes`** — never ask. Authorization policy, the pod's own confinement and the resource budgets
  are what constrain the agent; the sandbox floor is waived in this profile (see `--no-sandbox`
  below), so the pod boundary is carrying that weight. The shipped manifest uses this, because a deployed pod normally has nobody
  attached to it.
- **`--remote-approval`** — a human answers each guarded effect at `GET /approvals` and
  `POST /approvals/{id}`. An effect nobody answers within `FLUX_APPROVAL_TIMEOUT_SECS` (default 120)
  is **denied**, so a deployment nobody is watching refuses everything while still looking healthy.

Swap one for the other in the container's arguments; do not pass both, which flux rejects as
contradictory instructions.

One limit to know before you choose the second: until **C-687** (the supervisor authorization model)
lands, `--remote-approval` supports only the shared operator token or an open loopback bind. Everyone
who holds `flux-agent-token` can answer approvals, so you get a human in the loop but not
per-operator accountability, and per-request principal auth is refused alongside it rather than
silently accepted.

## What survives a restart

The pod's filesystem is read-only; only the volume persists. `--store /srv/flux/state` puts the
session store there, so a restart keeps:

- recorded sessions and their conversation history — `events.db`, which is what `flux sessions`,
  `replay` and `fork` read;
- flow state in `flow.db`;
- the agent's working directory at `/srv/flux/workspace`, on the same claim under a second subPath;
- anything under `$HOME/.flux`, which the manifest also points at the volume.

A restart **does not** keep:

- **in-flight turns.** A turn interrupted mid-flight does not resume; the client sees the stream end.
  `terminationGracePeriodSeconds: 60` gives a turn room to finalize during an ordinary rollout, but a
  killed pod is a lost turn.
- **pending approvals** under `--remote-approval`. The queue is in memory; a restart drops every
  parked effect, and callers see their calls denied rather than answered.
- **`/tmp`**, which is an `emptyDir`.

Because the store is single-writer SQLite on a `ReadWriteOnce` claim, the deployment is one replica
with the `Recreate` strategy. That is the supported topology, not a starting point to scale out of.

## Channel endpoints are a separate decision

The base serves flux's built-in coding agent, which declares no channels. If you deploy a **program**
instead — `flux app run <program.flux> --serve` — the program's declared channels are *not* on the
agent port and are *not* published by this Service:

- A `channel … { kind = "webhook" }` binds the address its own declaration names, with its own
  authentication (a `token secret "KEY"` and/or a signature verifier). Like the agent listener, it
  refuses a non-loopback bind with nothing to authenticate a caller.
- A Slack or cron channel opens no inbound port of its own at all.

Exposing a webhook channel is therefore a deliberate, separate act: add its container port, add a
port to the Service, and add a NetworkPolicy ingress rule naming who may reach it. Doing nothing
leaves the channel unreachable from outside the pod, which is the right default — a webhook endpoint
and an agent endpoint have different callers, different secrets and different blast radii, and
publishing them together because they share a pod is how one gets exposed by accident.

## Operational notes

- **Egress cannot be denied.** The model call happens in the pod. The shipped policy allows 443 to
  the public internet and excludes cloud metadata (`169.254.0.0/16`) and the private ranges; narrow
  it to your provider's published ranges or an egress proxy if you can.
- **`--no-sandbox` is required, and removing it does not harden anything.** A serving surface is
  pinned to the fail-closed sandbox floor and bubblewrap cannot create a user namespace inside an
  ordinary container, so the agent would simply refuse to start. The pod is the isolation boundary,
  which is why the profile runs read-only, non-root, with all capabilities dropped and no
  service-account token. Flux prints the resolved UNCONFINED posture at startup; keep that line in
  your audit logs.
- **NetworkPolicy is enforced by the CNI.** On a cluster whose CNI ignores it, these objects apply
  cleanly and restrict nothing.
- **`kubectl port-forward` does not traverse the ingress policy.** The kubelet opens the connection
  on the node, so your laptop reaches the pod even though the policy admits only labelled clients.

## See also

- [Agent-to-agent (A2A)](a2a.md) — the protocol and the client.
- [HTTP API](http-api.md) — every route this deployment serves.
- [Server authentication](../security/server-auth.md) — shared-token and principal modes.
- [Remote-system deployment](../remote-system-deployment.md) — the other axis: move the effects, keep
  the agent.
