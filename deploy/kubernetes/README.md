# Kubernetes profile

A Kustomize base that runs `flux system serve` as an ordinary workload. Raw manifests, no chart:
there is one deployment shape here and nothing to template, so a chart would add a values file
between an operator and four fields they are going to read anyway.

The Kubernetes plugin is not involved. This is not pod placement for agent workers; it is the remote
execution system, deployed like any other stateful service.

## Create the two Secrets first

They are deliberately not in this base. A checked-in manifest is exactly where a bearer token and a
TLS private key must never be, and a Kustomize `secretGenerator` reading a local file would only move
the problem into a path that is easy to commit by accident.

```sh
kubectl create namespace flux-system

# A certificate whose SAN matches the URL the client will use.
kubectl -n flux-system create secret tls flux-system-tls \
  --cert=tls.crt --key=tls.key

# A long random bearer token. Generate it where it will not land in shell history.
kubectl -n flux-system create secret generic flux-system-token \
  --from-file=token=/dev/stdin <<<"$(openssl rand -base64 39)"
```

## Apply

```sh
kubectl kustomize deploy/kubernetes | kubectl apply --dry-run=client -f -   # check first
kubectl apply -k deploy/kubernetes
kubectl -n flux-system rollout status deployment/flux-system
```

## Connect

The Service is ClusterIP, so the client runs in the cluster or reaches it through a tunnel:

```sh
kubectl -n flux-system port-forward service/flux-system 8790:8790
export FLUX_REMOTE_SYSTEM_TOKEN=$(kubectl -n flux-system get secret flux-system-token \
  -o jsonpath='{.data.token}' | base64 -d)
flux tui --remote https://127.0.0.1:8790 --remote-ca ca.pem --allow-private-net
```

A port-forwarded endpoint resolves to a loopback address, so the client needs its explicit
`--allow-private-net` grant. The certificate's SAN has to match the URL you actually type — a
certificate issued for the in-cluster Service name will not validate against `127.0.0.1`.

The `flux-system-tls` Secret above almost always holds a certificate issued by a cluster or
operator CA rather than a public one, which is what `--remote-ca ca.pem` is trusting. To reach the
same deployment *by name* instead, declare the CA on the binding — a `[[host]]` entry takes
`ca_cert` for exactly this:

```toml
[[host]]
id = "cluster"
backend = "kubernetes"
url = "https://127.0.0.1:8790"
credential_ref = "env/FLUX_REMOTE_SYSTEM_TOKEN"
ca_cert = "ca.pem"
grant = ["operator"]
```

Then `flux host probe cluster` verifies the endpoint against that CA, and `flux --host cluster …`
executes on it. An unreadable or malformed `ca_cert` refuses the binding by name; it never falls
back to the public trust store. See [Host bindings](../../website/docs/reference/config.md#host-bindings-host).

## What each file is for

| File | Why it exists |
|---|---|
| `kustomization.yaml` | The base itself: which manifests are applied, the shared labels, and the published image and tag that name the release this profile runs. `newName` is `ghcr.io/codewandler/flux-system`, which every release publishes; `newTag` is restamped by the cut. Neither is hand-maintained — see [deploy/README.md](../README.md) for how to verify the image's provenance or build it yourself. |
| `namespace.yaml` | A boundary the default-deny NetworkPolicy can own; enforces the restricted Pod Security Standard. |
| `workspace-pvc.yaml` | The canonical workspace and its delivery ledger. Losing it loses both. |
| `deployment.yaml` | One replica, `Recreate`, non-root, seccomp `RuntimeDefault`, read-only rootfs, all capabilities dropped, TCP probes. |
| `service.yaml` | ClusterIP on the named `https` port. |
| `networkpolicy.yaml` | Default-deny for the namespace, then ingress from a labelled client and DNS egress. |

## Things that will bite you

- **NetworkPolicy is enforced by the CNI.** On a cluster whose CNI ignores it, these objects apply
  cleanly and restrict nothing. Check before treating the default-deny as a boundary.
- **Egress is denied except DNS.** Guarded network effects executed on this substrate need
  destinations you add deliberately. There is no honest generic answer for what they should be.
- **One replica per workspace.** The delivery ledger that stops an operation id from executing twice
  is a file in the workspace. Two replicas sharing a volume is not a coordination mechanism, which is
  why the strategy is `Recreate` and the claim is `ReadWriteOnce`.
- **`fsGroup` is not decoration.** Without it a freshly provisioned volume is root-owned and the
  non-root daemon cannot write its own ledger.
- **The probes are TCP because every route is authenticated.** There is no unauthenticated health
  endpoint by design. An HTTP probe would need the bearer token in the manifest, which is the thing
  the Secret exists to avoid.
