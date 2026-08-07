# Agent-surface profile (Kubernetes)

A Kustomize base that runs **`flux app run --serve`** — the agent surface — as an ordinary
Kubernetes workload. The operator-facing guide is
[website/docs/agent/deployment.md](../../website/docs/agent/deployment.md); this file is the
contributor's map of what is in this directory and what each manifest promises.

## This is the other axis

`deploy/kubernetes/` and this directory look similar and are not the same deployment
([docs/designs/operating-a-deployed-host.md](../../docs/designs/operating-a-deployed-host.md)):

| | `deploy/kubernetes/` — substrate | `deploy/agent/` — agent |
|---|---|---|
| the pod runs | `flux system serve` | `flux app run --serve` |
| what moves into the cluster | only where guarded effects land | the whole agent: planning, model calls, session |
| what stays on your machine | the runtime, model, credentials, approvals, session store | a thin client (`flux a2a`) |
| the pod needs a model credential | no | **yes** — the model call happens there |
| the pod needs egress | DNS only | DNS **and** the model provider's API |
| transport | TLS terminated by the daemon (`--cert`/`--key`) | plain HTTP + bearer token; TLS is the cluster's job |
| you reach it with | `--host <name>` / `--remote <url>` | `flux a2a <url>` |

Both are Flux deployments and neither is a substitute for the other. Deploying both is normal; they
use separate namespaces so one default-deny NetworkPolicy per axis stays meaningful.

**Flux does not provision the cluster.** Nothing here creates a cluster, a node pool, a storage
class or an ingress controller. This base configures infrastructure you already have — the same
boundary [deploy/README.md](../README.md) states for every other profile.

## One image, two programs

There is no `flux-agent` image. This profile runs the image
[`deploy/container/Dockerfile`](../container/Dockerfile) already builds and
[`deploy/container/build-image.sh`](../container/build-image.sh) already stamps with a release
version, and selects the agent surface with a `command:` override:

```yaml
command: [/usr/local/bin/flux, app, run]
```

The image tag in `kustomization.yaml` is the same tag `deploy/kubernetes/kustomization.yaml` pins,
and `crates/flux-cli/tests/deployment_artifacts.rs` derives one from the other so they cannot drift.
A second image would be a second thing to build, attest, publish and get wrong; the provenance path
in [deploy/README.md](../README.md) covers this profile unchanged.

## Create the two Secrets first

They are deliberately not in this base. A checked-in manifest is exactly where a bearer token and a
model-provider credential must never be.

```sh
kubectl create namespace flux-agent

# The bearer token every caller must present. Generate it where it will not land in shell history.
kubectl -n flux-agent create secret generic flux-agent-token \
  --from-file=token=/dev/stdin <<<"$(openssl rand -base64 39)"

# The credential the agent's own model calls use. `api-key` is the key name deployment.yaml reads.
kubectl -n flux-agent create secret generic flux-agent-model-credentials \
  --from-file=api-key=/dev/stdin <<<"$ANTHROPIC_API_KEY"
```

`deployment.yaml` names `ANTHROPIC_API_KEY`, which is what flux's default `sonnet` model alias
resolves against. For another provider, change the env var name to the one that provider reads
(`OPENAI_API_KEY`, `OPENROUTER_API_KEY`, the `AWS_*` chain) and add `--model <spec>` to the
container's `args:`. `flux auth status` inside the pod reports what it actually found.

Neither Secret is marked `optional`. A namespace missing one gets a container that will not start,
which is the failure you want.

## Apply

```sh
kubectl kustomize deploy/agent | kubectl apply --dry-run=client -f -   # check first
kubectl apply -k deploy/agent
kubectl -n flux-agent rollout status deployment/flux-agent
```

## What each file is for

| File | Why it exists |
|---|---|
| `kustomization.yaml` | The base itself: which manifests are applied, the shared labels, and the image tag — the same release tag the substrate profile pins. |
| `namespace.yaml` | A boundary the default-deny NetworkPolicy can own; enforces the restricted Pod Security Standard. |
| `state-pvc.yaml` | The session store (`events.db`, `flow.db`) and the agent's workspace, on one claim under two subPaths. Losing it loses every recorded session. |
| `deployment.yaml` | One replica, `Recreate`, the image entrypoint overridden to the agent surface, the approval posture chosen out loud, non-root, seccomp `RuntimeDefault`, read-only rootfs, all capabilities dropped, no service-account token, HTTP probes against the auth-exempt `/health`. |
| `service.yaml` | ClusterIP on the named `http` port. The agent endpoint only. |
| `networkpolicy.yaml` | Default-deny for the namespace, then: ingress from a labelled operator client, DNS egress, and 443 egress to the public internet excluding cloud metadata and private ranges. |

## The approval posture is a decision, and the manifest makes it

`flux app run --serve` with no program **refuses to start** without one of these
(`ServedApprovalPosture::select`, `crates/flux-cli/src/app_cmd.rs`) — an HTTP request has no
terminal to prompt at, so the choice cannot be defaulted:

- **`--yes`** — never ask. Authorization policy, the sandbox floor and the resource budgets are what
  constrain the agent. This is what `deployment.yaml` ships, because a pod has nobody attached to
  it by default.
- **`--remote-approval`** — a human answers each guarded effect at `GET /approvals` /
  `POST /approvals/{id}`. Silence for `FLUX_APPROVAL_TIMEOUT_SECS` (default 120) **denies**. Swap it
  in when somebody really is watching; a deployment nobody watches under this posture refuses
  everything slowly while looking healthy.

Until **C-687** (the supervisor authorization model) lands, `--remote-approval` supports only the
shared operator token or an open loopback bind. Everyone holding `flux-agent-token` can answer
approvals, so it buys you a human in the loop but not per-operator accountability, and principal
auth (`[server] introspect_url`) is refused outright alongside it.

## Things that will bite you

- **`--no-sandbox` is load-bearing, exactly as in the container profile.** A serving surface is
  pinned to the fail-closed sandbox floor, and bubblewrap cannot create a user namespace inside an
  ordinary container. Removing the flag does not harden the pod; the agent stops starting. The pod
  is the isolation boundary, which is why this profile runs read-only, non-root, with all
  capabilities dropped. Flux prints the resolved posture at startup:

  ```
  warning: confinement profile BYPASSED by --no-sandbox: HTTP/A2A serving surface is running
  UNCONFINED. Sandbox network controls cannot apply; provide equivalent isolation in an outer
  container/VM and retain this startup line in operator audit logs.
  ```

- **The listener is plain HTTP.** `flux app run --serve` terminates no TLS. The bearer token
  authenticates the caller to the agent and does nothing in the other direction; anyone who can read
  the connection can replay it. Keep the hop inside a boundary you trust, and terminate TLS at an
  ingress if the endpoint leaves the cluster.

- **Egress cannot be denied here.** The model call happens in the pod, so the substrate profile's
  DNS-only egress would leave every turn hanging. `flux-agent-allow-model-api` is the compromise:
  443 to the public internet, with cloud metadata (`169.254.0.0/16`) and the private ranges
  excluded. Narrow it to your provider's published ranges or an egress proxy if you can.

- **`kubectl port-forward` bypasses the ingress policy.** The kubelet opens the connection on the
  node, so a port-forward reaches the pod even though `flux-agent-allow-operator` admits nobody who
  is not labelled. Convenient for an operator, and worth knowing before you treat the policy as the
  only door.

- **One replica per store.** `events.db` is SQLite in WAL mode on a `ReadWriteOnce` claim: a
  single-writer store, which is why the strategy is `Recreate` and the replica count is not a
  starting point to scale out of. Serve more concurrency with more sessions in one pod, or with a
  second deployment holding its own claim.

- **`fsGroup` is not decoration.** Without it a freshly provisioned volume is root-owned and the
  non-root agent cannot create its own store.

- **The probes are HTTP here, unlike the substrate profile.** The agent surface registers `/health`
  and the agent card outside its authentication layer — the documented health/discovery exemption —
  so a probe proves the router answers without a token ever entering a manifest. `flux system serve`
  has no such route, which is why its probes can only be TCP.

## Checks

```sh
# Artifact contract — runs in ordinary CI, no cluster needed. Covers both profiles.
cargo test -p flux-cli --test deployment_artifacts

# The profile renders and validates against real API schemas.
kubectl kustomize deploy/agent | kubectl apply --dry-run=client -f -
```
