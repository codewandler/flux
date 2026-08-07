# VM / microVM guest profile

A hardened service unit and an install contract for a guest that serves `flux system serve`. A VM and
a microVM use the same contract — the difference is how the guest was created, and Flux is not
involved in that.

## What this profile does not do

It does not create, pool, snapshot, attest, resume or destroy a guest. Firecracker, Cloud Hypervisor,
Kata and cloud-specific lifecycle verbs are provisioning concerns outside the shipped runtime. This
profile configures a guest that already exists and gives it an endpoint. Once it has an address and
runs this daemon, it is a remote execution system like any other.

## Install

```sh
sudo ./install-flux-system.sh --version 0.58.0
```

Fetches the published `flux-cli-x86_64-unknown-linux-gnu.tar.xz` for that tag, verifies it against its
published `.sha256` sidecar, creates the `flux` service identity, creates the directories and file
modes below, and installs the unit. `--binary PATH` installs a binary you already have, for an
air-gapped guest.

The script is idempotent, so re-running it with a newer `--version` is the upgrade path. It keeps the
previous binary at `/usr/local/bin/flux.previous`, which is the rollback.

`cloud-init.yaml` is the same install contract expressed as guest bootstrap: durable workspace disk,
packages, the unit, an nftables ruleset, then the install script. Every `REPLACE_ME` is required.

## Files, owners and modes

| Path | Owner | Mode | Why |
|---|---|---|---|
| `/usr/local/bin/flux` | `root:root` | `0755` | The service identity must not be able to replace its own binary. |
| `/srv/flux/workspace` | `flux:flux` | `0750` | The canonical workspace, and the one path the unit makes writable. Mount a durable disk here. |
| `/etc/flux/tls/tls.crt` | `root:*` | `0644` | Public. |
| `/etc/flux/tls/tls.key` | `root:flux` | `0640` | Readable by the service identity, by nobody else. |
| `/etc/flux/remote-system.env` | `root:root` | `0600` | `FLUX_REMOTE_SYSTEM_TOKEN=…`. systemd reads it as root before privileges drop, so the service never opens it. |

The token is never a command-line argument, so it cannot be read out of a process listing. The daemon
refuses to start on an empty one: a guest whose operator has not delivered a secret yet fails closed
rather than serving unauthenticated.

## Trusting the guest's certificate

`/etc/flux/tls/tls.crt` is normally issued by your own CA, not a public one, so a client has to be
told which CA to trust. A named binding takes that CA directly, which is how the guest becomes
reachable as `--host vm-guest` rather than only as `--remote … --remote-ca`:

```toml
[[host]]
id = "vm-guest"
backend = "microvm"
url = "https://guest.internal:8790"
credential_ref = "env/FLUX_REMOTE_SYSTEM_TOKEN"
ca_cert = "/etc/flux/guest-ca.pem"
grant = ["operator"]
```

`ca_cert` is the path to the CA certificate **on the client machine** — a location, not the
certificate itself, and not a secret. `flux host probe vm-guest` verifies the guest against it and
reports the negotiated protocol version. An unreadable or malformed certificate refuses the binding
and names it; nothing falls back to the public trust store, and no flag relaxes that. See
[Host bindings](../../website/docs/reference/config.md#host-bindings-host).

## The sandbox floor

This profile does **not** pass `--no-sandbox`, and that is the substantive reason to run a guest
rather than a container. A guest owns its kernel, so bubblewrap can create the user namespace the
fail-closed sandbox floor needs, and spawned processes are confined inside the guest as well as by it.
Install `bubblewrap` — `cloud-init.yaml` does.

If a stripped microVM kernel has no unprivileged user namespaces, the daemon refuses to start and says
so. Adding `--no-sandbox` to `ExecStart` then makes the guest boundary the isolation, as a deliberate
and recorded decision rather than a silent degradation.

## Firewall

TCP 8790 must be admitted only from intended clients, at the guest firewall *and* in front of it. The
`cloud-init.yaml` nftables ruleset drops everything else. A remote execution system on an open port is
one stolen bearer token away from being someone else's execution system.

## Verify the unit

```sh
systemd-analyze verify deploy/vm/flux-system.service
systemd-analyze security --offline=true deploy/vm/flux-system.service
```

Two hardening directives are deliberately absent, and the unit says why at the point of omission:
`RestrictNamespaces` would disable the bubblewrap floor this profile exists to keep, and
`SystemCallFilter` cannot be both wide enough for arbitrary approved workloads and narrow enough to be
a boundary.
