---
title: Deploy a remote execution system
description: "The production contract and BYO deployment profiles for running flux system serve in a container, Kubernetes pod, VM, or microVM."
---

# Deploy a remote execution system

`flux system serve` is the shipped placement primitive for keeping the model, policy, and approval UI
local while file, process, and network effects happen elsewhere. The daemon is deliberately unaware
of how its host was provisioned. The profiles below are **BYO deployment profiles** for an
operator-supplied container, Kubernetes pod, VM, or microVM; Flux does not yet ship an OCI image,
Helm chart, Kubernetes operator, or microVM provisioner.

## The contract every profile must satisfy

| Concern | Required deployment contract |
|---|---|
| Version | Pin the same Flux release on client and daemon. The protocol is versioned and rejects incompatible peers. |
| Workspace | Mount one durable directory and pass it as `--workspace`. This canonical workspace is the tree every remote file and process operation sees. There is no client-side synchronization. |
| TLS certificate | Terminate TLS in `flux system serve` with `--cert` and `--key`. Use a certificate whose SAN matches the client URL; pass `--remote-ca` only for a private CA. |
| Bearer token | Put a long random value in `FLUX_REMOTE_SYSTEM_TOKEN`, or in the environment variable named by `--token-env`. Never put it in an image, URL, command-line literal, or checked-in manifest. |
| Network | Admit the listener only from intended clients. A private/loopback client target additionally needs its explicit `--allow-private-net` grant. |
| Persistence | Keep the workspace volume. The bounded persistent delivery ledger lives at `.flux/remote-system-delivery.json` beneath it and prevents an operation id from executing twice. |
| Readiness | A TCP probe proves the TLS listener accepts connections. A fully authenticated `/system/v1/handshake` from a trusted operator probe proves protocol readiness. Do not publish the bearer token in a probe specification. |
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

## Container profile

Flux does not publish an official image yet. Build or supply an image containing the pinned `flux`
binary, a CA bundle, a TCP health-probe utility such as `nc`, and a non-root runtime user. Mount
rather than bake the workspace, TLS private key, and token. The image entrypoint should read the
token from the orchestrator's secret mount into the daemon environment and then `exec` the argv-only
command above.

A deployment equivalent to the following is the required shape (replace the image and secret
mechanism with your own):

```yaml
services:
  flux-system:
    image: your-registry.example/flux-system:VERSION
    user: "10001:10001"
    read_only: true
    ports: ["127.0.0.1:8790:8790"]
    volumes:
      - flux-workspace:/srv/flux/workspace
      - ./tls:/run/flux-tls:ro
      - ./secrets:/run/flux-secrets:ro
    tmpfs: ["/tmp"]
    entrypoint: ["/bin/sh", "-ec"]
    command:
      - |
        export FLUX_REMOTE_SYSTEM_TOKEN="$$(cat /run/flux-secrets/token)"
        exec flux system serve --workspace /srv/flux/workspace --bind 0.0.0.0:8790 \
          --cert /run/flux-tls/tls.crt --key /run/flux-tls/tls.key
    healthcheck:
      test: ["CMD", "nc", "-z", "127.0.0.1", "8790"]
      interval: 10s
      timeout: 2s
      retries: 6
volumes:
  flux-workspace: {}
```

The loopback host publish in this example expects a tunnel or same-host client. For direct remote
clients, bind an appropriate interface and enforce source restrictions in the host firewall. Do not
mount the Docker socket unless the remote workspace's approved operations genuinely require host-
equivalent Docker authority.

## Kubernetes pod profile

The Kubernetes plugin is not involved in execution. Deploy the same daemon as an ordinary workload
with a PersistentVolumeClaim for the canonical workspace, a TLS Secret, a separately mounted bearer
Secret, a ClusterIP Service, and a NetworkPolicy that admits only the control plane or gateway that
will connect.

The pod-level requirements are:

```yaml
apiVersion: apps/v1
kind: Deployment
metadata: {name: flux-system}
spec:
  replicas: 1
  strategy: {type: Recreate}
  selector: {matchLabels: {app: flux-system}}
  template:
    metadata: {labels: {app: flux-system}}
    spec:
      securityContext:
        runAsNonRoot: true
        seccompProfile: {type: RuntimeDefault}
      containers:
        - name: flux-system
          image: your-registry.example/flux-system:VERSION
          command: ["/bin/sh", "-ec"]
          args:
            - |
              export FLUX_REMOTE_SYSTEM_TOKEN="$(cat /run/flux-token/token)"
              exec flux system serve --workspace /srv/flux/workspace --bind 0.0.0.0:8790 \
                --cert /run/flux-tls/tls.crt --key /run/flux-tls/tls.key
          ports: [{name: https, containerPort: 8790}]
          readinessProbe:
            tcpSocket: {port: https}
            periodSeconds: 5
          livenessProbe:
            tcpSocket: {port: https}
            periodSeconds: 15
          securityContext:
            allowPrivilegeEscalation: false
            readOnlyRootFilesystem: true
            capabilities: {drop: ["ALL"]}
          volumeMounts:
            - {name: workspace, mountPath: /srv/flux/workspace}
            - {name: tls, mountPath: /run/flux-tls, readOnly: true}
            - {name: token, mountPath: /run/flux-token, readOnly: true}
            - {name: tmp, mountPath: /tmp}
      volumes:
        - name: workspace
          persistentVolumeClaim: {claimName: flux-system-workspace}
        - name: tls
          secret: {secretName: flux-system-tls}
        - name: token
          secret: {secretName: flux-system-token}
        - name: tmp
          emptyDir: {}
```

Use one replica per canonical workspace. The delivery ledger is local to that workspace; active-
active replicas sharing a volume are not a supported coordination mechanism. Put a Service in front
of the named `https` port and restrict it with a NetworkPolicy. If an ingress or service mesh
re-terminates TLS, keep authenticated encryption and the expected certificate identity intact from
the client to the daemon; the bearer token is not a substitute for server authentication.

## VM or microVM profile

A VM and a microVM use the same daemon contract. Provision the guest by your normal mechanism,
attach a durable workspace disk, install the pinned Flux binary, copy a TLS certificate/key, and
restrict TCP 8790 at the guest and infrastructure firewall. Then supervise the daemon, for example:

```ini
[Unit]
Description=Flux remote execution system
After=network-online.target
Wants=network-online.target

[Service]
User=flux
Group=flux
EnvironmentFile=/etc/flux/remote-system.env
ExecStart=/usr/local/bin/flux system serve --workspace /srv/flux/workspace --bind 0.0.0.0:8790 --cert /etc/flux/tls/tls.crt --key /etc/flux/tls/tls.key
Restart=on-failure
RestartSec=2
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ReadWritePaths=/srv/flux/workspace

[Install]
WantedBy=multi-user.target
```

Store `FLUX_REMOTE_SYSTEM_TOKEN=…` in `/etc/flux/remote-system.env` with root ownership and mode
`0600`; make the TLS key readable only by the service identity. A stronger VM boundary does not
remove the need for workspace confinement, argv-only spawning, output caps, TLS, or authorization
and approval on the client.

Flux does not create, pool, snapshot, attest, resume, or destroy Firecracker, Cloud Hypervisor,
Kata, or other microVMs. Those are provisioning concerns outside the shipped runtime. Once the guest
has an address and runs this daemon, it is a remote execution system like any other.

## Operation compatibility and trust boundary

Port-aware core file, search, edit, process, and network operations use the selected remote system.
Operations that still own native-only handles are hidden and refused. Native integrations and
plugins are also disabled in remote mode today; there is **no local fallback**. Run such an
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
- Alert on restarts and reconcile `Unknown` mutation outcomes before retrying.
- Treat the remote as able to observe its filesystem, process arguments, returned bytes, and any
  operation-bound secret deliberately sent to it.

## Related docs

- [Execution topologies](./topologies.md) — choose local effects, remote effects, or a served agent.
- [Docker plugin](./plugins/docker.md) — manage an existing Docker daemon; not a placement backend.
- [Kubernetes plugin](./plugins/kubernetes.md) — manage an existing cluster; not a pod runtime.
- [Safety](./agent/safety.md) — authorization, approval, and guarded IO.
