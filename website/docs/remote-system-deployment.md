---
title: Deploy a remote execution system
description: "Shipped deployment profiles — OCI image, Kubernetes manifests and a microVM guest unit — for running flux system serve in a container, Kubernetes pod, VM, or microVM."
---

# Deploy a remote execution system

`flux system serve` is the shipped placement primitive for keeping the model, policy, and approval UI
local while file, process, and network effects happen elsewhere. The daemon is deliberately unaware
of how its host was provisioned.

Flux now ships the deployment artifacts for it, in [`deploy/`](https://github.com/codewandler/flux/tree/main/deploy):
an OCI image build, a Kubernetes Kustomize base, and a VM/microVM guest unit with an install contract.
They remain **BYO deployment profiles** in the sense that matters: Flux ships the artifacts and you
bring the infrastructure. Flux does not create, pool, snapshot, attest, resume or destroy Docker
hosts, clusters, Firecracker, Cloud Hypervisor or Kata microVMs, and it does not intend to on the
remote wire — those lifecycle verbs belong behind a generic isolation-provisioner contract. Each
profile configures infrastructure you already have.

## The contract every profile satisfies

| Concern | Required deployment contract |
|---|---|
| Version | Pin the same Flux release on client and daemon. The protocol is versioned and rejects incompatible peers. |
| Workspace | Mount one durable directory and pass it as `--workspace`. This canonical workspace is the tree every remote file and process operation sees. There is no client-side synchronization. |
| TLS certificate | Terminate TLS in `flux system serve` with `--cert` and `--key`. Use a certificate whose SAN matches the client URL; pass `--remote-ca` only for a private CA. |
| Bearer token | Put a long random value in `FLUX_REMOTE_SYSTEM_TOKEN`, or in the environment variable named by `--token-env`. Never put it in an image, URL, command-line literal, or checked-in manifest. |
| Network | Admit the listener only from intended clients. A private/loopback client target additionally needs its explicit `--allow-private-net` grant. |
| Persistence | Keep the workspace volume. The bounded persistent delivery ledger lives at `.flux/remote-system-delivery.json` beneath it and prevents an operation id from executing twice. |
| Readiness | A TCP probe proves the TLS listener accepts connections. A fully authenticated `/system/v1/handshake` from a trusted operator probe proves protocol readiness. Do not publish the bearer token in a probe specification. |
| Isolation | A serving surface defaults to a fail-closed sandbox floor. Satisfy it, or waive it explicitly — see [The sandbox floor](#the-sandbox-floor). |
| Shutdown | Give the daemon time to stop accepting work and preserve the workspace volume. A link loss after acceptance can produce an honest `Unknown` outcome, so mutations are not automatically retried. |

The base command is the same everywhere:

```sh
export FLUX_REMOTE_SYSTEM_TOKEN='generate-a-long-random-token'
flux system serve --workspace /srv/flux/workspace --bind 0.0.0.0:8790 \
  --cert /run/flux-tls/tls.crt --key /run/flux-tls/tls.key
```

Connect from the control machine:

```sh
export FLUX_REMOTE_SYSTEM_TOKEN='the-same-token'
flux tui --remote https://worker.example:8790 --remote-ca worker-ca.pem
```

Use a publicly trusted certificate by omitting `--remote-ca`. If the endpoint resolves to a private
address, add the explicit global `--allow-private-net` option on the client. Local mode remains the
default when `--remote` is absent.

## The sandbox floor

`flux system serve` is a serving surface, so it defaults to a **fail-closed** sandbox floor: with no
usable bubblewrap backend it refuses to start rather than running unconfined by accident. The shipped
profiles answer that requirement differently, on purpose.

| Profile | Answer | Why |
|---|---|---|
| Container, Kubernetes pod | `--no-sandbox` | Creating a user namespace inside an ordinary container is refused under the default seccomp profile, so bubblewrap cannot run there. The container is the isolation boundary instead — which is why these profiles run read-only, non-root, with all capabilities dropped and ingress restricted. |
| VM / microVM guest | keeps the floor | A guest owns its kernel, so bubblewrap works and spawned processes are confined *inside* the guest as well as by it. This is the substantive reason to run a guest. |

`--no-sandbox` is that floor's documented outer-container/VM escape, and the daemon prints a prominent
UNCONFINED posture at startup. Removing the flag from a container profile does not harden it; it
produces a daemon that refuses to start. That is the intended failure, not a bug to route around.

## Container profile

Every release publishes the image. Pull it:

```sh
docker pull ghcr.io/codewandler/flux-system:0.58.0
```

The image runs only `flux system serve`, as uid 10001, and carries no bearer token, no TLS private key
and no workspace content in any layer — all three are mounted at run time. Its version label and tag
come from the same workspace version every other release entry point reads.

It is built by repacking the *released* `flux-cli-x86_64-unknown-linux-gnu.tar.xz` — the archive the
release already attested and published, re-checked against its `.sha256` sidecar — so the binary in
the layer is the binary the release workflow attested, and both carry provenance you can check:

```sh
gh attestation verify oci://ghcr.io/codewandler/flux-system:0.58.0 --repo codewandler/flux

gh release download v0.58.0 --pattern 'flux-cli-x86_64-unknown-linux-gnu.tar.xz'
gh attestation verify flux-cli-x86_64-unknown-linux-gnu.tar.xz --repo codewandler/flux
```

To build the same image locally instead — an air-gapped registry, or a different base image — use
the script the release job runs:

```sh
deploy/container/build-image.sh --release 0.58.0
```

Run it with the workspace, TLS material and token mounted rather than baked:

```yaml
services:
  flux-system:
    image: ghcr.io/codewandler/flux-system:0.58.0
    read_only: true
    ports: ["127.0.0.1:8790:8790"]
    volumes:
      - flux-workspace:/srv/flux/workspace
      - ./tls:/run/flux-tls:ro
    env_file: ./secrets/remote-system.env    # FLUX_REMOTE_SYSTEM_TOKEN=…
    tmpfs: ["/tmp"]
volumes:
  flux-workspace: {}
```

The image's own entrypoint and arguments already carry the mount paths and `--no-sandbox`, so nothing
above overrides them. An `env_file` keeps the token off the command line, where a process listing
would expose it. The user is baked into the image, so `user:` is not needed and overriding it will
break workspace ownership.

The loopback host publish in this example expects a tunnel or same-host client. For direct remote
clients, bind an appropriate interface and enforce source restrictions in the host firewall. Do not
mount the Docker socket unless the remote workspace's approved operations genuinely require
host-equivalent Docker authority.

## Kubernetes pod profile

The Kubernetes plugin is not involved in execution. The Kustomize base at
[`deploy/kubernetes/`](https://github.com/codewandler/flux/tree/main/deploy/kubernetes) deploys the
same daemon as an ordinary workload: one replica, `Recreate`, a PersistentVolumeClaim for the
canonical workspace, a TLS Secret, a separately mounted bearer Secret, a ClusterIP Service, TCP
readiness and liveness probes, non-root with `seccompProfile: RuntimeDefault` and a read-only root
filesystem, and a default-deny NetworkPolicy.

```sh
kubectl create namespace flux-system
kubectl -n flux-system create secret tls flux-system-tls --cert=tls.crt --key=tls.key
kubectl -n flux-system create secret generic flux-system-token \
  --from-file=token=/dev/stdin <<<"$(openssl rand -base64 39)"

kubectl kustomize deploy/kubernetes | kubectl apply --dry-run=client -f -
kubectl apply -k deploy/kubernetes
```

Both Secrets are created out of band and are deliberately absent from the base — a checked-in manifest
is exactly where a bearer token and a TLS private key must never be.

Use one replica per canonical workspace. The delivery ledger is local to that workspace; active-active
replicas sharing a volume are not a supported coordination mechanism. The NetworkPolicy denies
everything by default and then admits a labelled client plus DNS; egress for the guarded network
effects you intend to run there is yours to add deliberately. If an ingress or service mesh
re-terminates TLS, keep authenticated encryption and the expected certificate identity intact from the
client to the daemon; the bearer token is not a substitute for server authentication.

## VM or microVM profile

A VM and a microVM use the same daemon contract. The guest artifacts are
[`deploy/vm/`](https://github.com/codewandler/flux/tree/main/deploy/vm): a hardened service unit, an
idempotent install contract, and a cloud-init profile that expresses the same contract as guest
bootstrap.

```sh
sudo deploy/vm/install-flux-system.sh --version 0.58.0
```

That fetches and checksum-verifies the pinned release archive, creates the non-root `flux` service
identity, creates the durable workspace at `/srv/flux/workspace`, sets the secret file modes below,
and installs the unit.

| Path | Owner | Mode |
|---|---|---|
| `/usr/local/bin/flux` | `root:root` | `0755` |
| `/srv/flux/workspace` | `flux:flux` | `0750` |
| `/etc/flux/tls/tls.key` | `root:flux` | `0640` |
| `/etc/flux/remote-system.env` | `root:root` | `0600` |

`cloud-init.yaml` additionally partitions and mounts a durable workspace disk, installs bubblewrap and
an nftables ruleset that drops everything except TCP 8790 from the client range. Admit that port only
from intended clients, at the guest firewall and in front of it.

The unit sets `NoNewPrivileges`, `ProtectSystem=strict` with the workspace as the sole
`ReadWritePaths`, `PrivateTmp`, `PrivateDevices`, `ProtectHome`, `ProtectProc=invisible`, the kernel
protections, and `RestrictAddressFamilies`. It deliberately omits `RestrictNamespaces`, which would
disable the bubblewrap floor this profile keeps, and `SystemCallFilter`, which cannot be both wide
enough for arbitrary approved workloads and narrow enough to be a boundary.

Flux does not create, pool, snapshot, attest, resume, or destroy Firecracker, Cloud Hypervisor, Kata,
or other microVMs. Those are provisioning concerns outside the shipped runtime. Once the guest has an
address and runs this daemon, it is a remote execution system like any other.

## Upgrade, rollback, and protocol mismatch

**The client and the daemon must be the same release.** The protocol version is a plain equality
check with no negotiation window, so a mixed pair does not degrade — it refuses.

A client whose release disagrees with the daemon's fails at connect, before any operation is sent:

```
remote-system protocol mismatch: local 3, remote 2
```

The daemon enforces the same rule per request, answering `400 Bad Request` with
`unsupported remote-system protocol version <n>`. Nothing executes on a mismatched pair in either
direction, so an upgrade cannot half-apply.

**Upgrade order.** Stop the client, replace the daemon, then replace the client. A brief refusal
window between the two is the expected behaviour and is safe; work in flight is not silently retried.

| Profile | Upgrade | Rollback |
|---|---|---|
| Container | `docker pull ghcr.io/codewandler/flux-system:<new>` (or `deploy/container/build-image.sh --release <new>`), then recreate the container against the same workspace volume. | Recreate against the previous image tag. The volume is untouched by either. |
| Kubernetes | Change `newTag` in `deploy/kubernetes/kustomization.yaml`, `kubectl apply -k`. `Recreate` guarantees the old pod releases the volume first. | `kubectl rollout undo deployment/flux-system`, or reapply the previous tag. |
| VM / microVM | Re-run `install-flux-system.sh --version <new>`, then `systemctl restart flux-system`. | `cp /usr/local/bin/flux.previous /usr/local/bin/flux && systemctl restart flux-system`. |

**The workspace survives all of this, and so does the delivery ledger.** Both live on the mounted
volume or disk, not in the image or the guest root. A restart converts any operation that was accepted
but never answered into an honest `Unknown` rather than replaying it, so an upgrade cannot cause an
effect to land twice. Reconcile those before retrying.

Downgrading past a protocol version change is a client-and-daemon operation, never a daemon-only one.
The workspace format is not versioned by the protocol, so a rolled-back daemon reads the same
workspace and the same ledger.

## Operation compatibility and trust boundary

The live catalog declares one of three compatibility placements for every production operation:
`local-control-plane` work intentionally stays on the coordinator; `selected-execution-system`
operations send guarded effects to the selected remote system; and `native-system-only` operations
are hidden and refused. Unannotated downstream operations default to `native-system-only` and fail
closed. There is **no local fallback**. Native integrations and plugins are classified that way
today. Run such an
integration where its authority exists, or use the served-agent topology until remote-capable plugin
placement is implemented.

Authorization, approval, model selection, provider credentials, and the session/evidence store stay
on the control machine. Physical path confinement, process sandboxing, egress enforcement, and
operation-bound credentials act on or cross into the remote host. Returned results are remotely
reported, not independently observed by the local runtime. See the complete
[guarantees table](./topologies.md#which-guarantees-cross-the-link).

## Production checklist

- Pin identical Flux releases and test the authenticated handshake before admitting work.
- Persist and back up the canonical workspace, including its delivery ledger.
- Use a CA-issued or explicitly pinned private-CA certificate with the correct SAN.
- Generate and rotate a high-entropy bearer token; never place it in a URL or image layer.
- Restrict ingress to intended clients and keep the daemon off the public Internet.
- Run as a dedicated non-root identity and apply the host/container/pod sandbox independently.
- Know which answer your profile gives the sandbox floor, and why.
- Alert on restarts and reconcile `Unknown` mutation outcomes before retrying.
- Treat the remote as able to observe its filesystem, process arguments, returned bytes, and any
  operation-bound secret deliberately sent to it.

## Related docs

- [Execution topologies](./topologies.md) — choose local effects, remote effects, or a served agent.
- [Docker plugin](./plugins/docker.md) — manage an existing Docker daemon; not a placement backend.
- [Kubernetes plugin](./plugins/kubernetes.md) — manage an existing cluster; not a pod runtime.
- [Safety](./agent/safety.md) — authorization, approval, and guarded IO.
