# Deployment artifacts

These are the shipped deployment profiles for the two things Flux can move onto someone else's
machine. This file is the contributor's map of what is in the tree and what each artifact promises.

## The substrate: `flux system serve`

The daemon that keeps the model, policy and approval UI on your machine while file, process and
network effects land somewhere else. The operator-facing guide is
[website/docs/remote-system-deployment.md](../website/docs/remote-system-deployment.md).

| Profile | Artifacts | Isolation boundary |
|---|---|---|
| Container / OCI | [`container/Dockerfile`](container/Dockerfile), [`container/build-image.sh`](container/build-image.sh) | the container |
| Kubernetes | [`kubernetes/`](kubernetes/) (Kustomize base) | the pod |
| VM / microVM guest | [`vm/flux-system.service`](vm/flux-system.service), [`vm/install-flux-system.sh`](vm/install-flux-system.sh), [`vm/cloud-init.yaml`](vm/cloud-init.yaml) | the guest, plus bubblewrap inside it |

## The agent: `flux app run --serve`

The whole agent — planning, model calls, session — living in the cluster, reached with `flux a2a`.
Its operator-facing guide is
[website/docs/agent/deployment.md](../website/docs/agent/deployment.md), and
[`agent/README.md`](agent/README.md) explains how the two axes differ.

| Profile | Artifacts | Isolation boundary |
|---|---|---|
| Kubernetes | [`agent/`](agent/) (Kustomize base) | the pod |

It runs the **same image** as the container profile above — the agent surface is a different program
inside one released binary, selected by a `command:` override. There is no second image, so the
release identity and provenance path below cover it unchanged.

## What these artifacts are not

**Flux does not provision Docker hosts, clusters or microVMs.** Nothing here creates, pools,
snapshots, attests, resumes or destroys a machine. Firecracker, Cloud Hypervisor, Kata and every
cloud-specific lifecycle verb belong behind a future generic isolation-provisioner contract, not on
the remote wire. Each profile configures infrastructure you already have, and the daemon it starts is
deliberately unaware of how its host came to exist.

That boundary is what makes these artifacts composable: a host binding names an endpoint that one of
these profiles brought into existence, and admits only what the protocol handshake verifies.

## The sandbox floor, and why the profiles answer it differently

`flux system serve` is a serving surface, so it defaults to a fail-closed sandbox floor: without a
usable bubblewrap backend it refuses to start rather than running unconfined by accident. That is the
right default, and the profiles do not all satisfy it the same way.

- **Container and pod** pass `--no-sandbox`, and it is load-bearing. Creating a user namespace inside
  an ordinary container is refused under the default seccomp profile, so bubblewrap cannot run there;
  an image that depended on it would fail to start on most hosts. `--no-sandbox` is that floor's
  documented outer-container/VM escape. The container *is* the isolation boundary, which is why these
  profiles run read-only, non-root, with all capabilities dropped and ingress restricted. The daemon
  prints a prominent UNCONFINED posture at startup.
- **The VM/microVM guest** keeps the floor. A guest owns its kernel, so bubblewrap works and spawned
  processes are confined inside the guest as well as by it. This is the substantive reason to run a
  guest rather than a container.

An operator who removes `--no-sandbox` from the container profiles does not harden them; they get a
daemon that refuses to start. That is the intended failure.

## Release identity and provenance

Every release publishes the image. Pull it — nothing needs building:

```sh
docker pull ghcr.io/codewandler/flux-system:<version>
```

That is the reference both Kustomize bases pin as `newName`, so `kubectl apply -k deploy/kubernetes`
works against any cluster that can reach `ghcr.io`.

### What the published image contains

The release workflow's `publish-container-image` job repacks the *released* archive: the
`flux-cli-x86_64-unknown-linux-gnu.tar.xz` that the `attest` job already attested and the Release
already published, re-checked against its `.sha256` sidecar before it becomes a layer. It never
compiles anything. So one binary is described by both attestations — the archive's and the image's:

```sh
# The image, by digest, with its provenance statement stored beside it in the registry.
gh attestation verify oci://ghcr.io/codewandler/flux-system:<version> --repo codewandler/flux

# The archive inside it, if you want to check the binary independently.
gh release download v<version> --pattern 'flux-cli-x86_64-unknown-linux-gnu.tar.xz'
gh attestation verify flux-cli-x86_64-unknown-linux-gnu.tar.xz --repo codewandler/flux
```

### Building it yourself

Locally reproducing the published image is the same script the release job runs:

```sh
deploy/container/build-image.sh --release <version>
```

This downloads the published archive for that tag, checks it against its published `.sha256`
sidecar, and repacks that exact binary. `--staged DIR` is the same repack from a directory that
already holds the archive — that is the mode CI uses, where the bytes arrive as the checked asset
set rather than from a Release that does not exist yet.

`deploy/container/build-image.sh --print-image` prints the registry path, which is where that path
is written down: the publishing job checks its own `github.repository_owner` against it and refuses
to push if they have drifted, and `crates/flux-cli/tests/deployment_artifacts.rs` checks both
Kustomize profiles' `newName` against it.

`--binary PATH` builds from a binary you already have. It is for development and for the container
integration test, and it carries no release provenance; nothing built that way should be published.

The version comes from `[workspace.package].version` in the root `Cargo.toml` — the same one-liner
`scripts/cut-release.sh`, `release.yml` and `crates-io.yml` read — so the image tag, the
`org.opencontainers.image.version` label and both Kustomize image tags all name one release.
`scripts/cut-release.sh` restamps those two tags as part of the cut
(`scripts/stamp-deployment-images.sh`), and refuses to finish while any shipped manifest still names
the version being left behind.

The image is published to a registry rather than attached to the GitHub Release on purpose: the
release asset inventory is closed at 28 names and structurally enforced, and an image is not a
release asset.

## Checks

```sh
# Artifact contract — runs in ordinary CI, no Docker needed.
cargo test -p flux-cli --test deployment_artifacts

# The container profile end to end: build the image, mount a workspace/TLS/token, drive it with a
# client, restart it, prove the workspace and delivery ledger survived. Opt-in, because Docker is
# not available in ordinary workspace CI.
cargo build --target x86_64-unknown-linux-musl --bin flux
FLUX_TEST_CONTAINER=1 cargo test -p codewandler-flux-server --test remote_system_container

# Each Kubernetes profile renders and validates against real API schemas.
kubectl kustomize deploy/kubernetes | kubectl apply --dry-run=client -f -
kubectl kustomize deploy/agent | kubectl apply --dry-run=client -f -

# The guest unit parses and its hardening is what it claims.
systemd-analyze verify deploy/vm/flux-system.service
systemd-analyze security --offline=true deploy/vm/flux-system.service
```
