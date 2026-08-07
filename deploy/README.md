# Deployment artifacts for the remote execution system

These are the shipped deployment profiles for `flux system serve` — the daemon that keeps the model,
policy and approval UI on your machine while file, process and network effects land somewhere else.
The operator-facing guide is
[website/docs/remote-system-deployment.md](../website/docs/remote-system-deployment.md); this file is
the contributor's map of what is in the tree and what each artifact promises.

| Profile | Artifacts | Isolation boundary |
|---|---|---|
| Container / OCI | [`container/Dockerfile`](container/Dockerfile), [`container/build-image.sh`](container/build-image.sh) | the container |
| Kubernetes | [`kubernetes/`](kubernetes/) (Kustomize base) | the pod |
| VM / microVM guest | [`vm/flux-system.service`](vm/flux-system.service), [`vm/install-flux-system.sh`](vm/install-flux-system.sh), [`vm/cloud-init.yaml`](vm/cloud-init.yaml) | the guest, plus bubblewrap inside it |

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

An image is built from a release, not alongside one:

```sh
deploy/container/build-image.sh --release 0.58.0
```

This downloads the published `flux-cli-x86_64-unknown-linux-gnu.tar.xz` for that tag, checks it
against its published `.sha256` sidecar, and repacks that exact binary into the image. The bytes in
the layer are the bytes the release workflow's `actions/attest` step attested, so
`gh attestation verify` against the archive covers the binary in the image:

```sh
gh release download v0.58.0 --pattern 'flux-cli-x86_64-unknown-linux-gnu.tar.xz'
gh attestation verify flux-cli-x86_64-unknown-linux-gnu.tar.xz --repo codewandler/flux
```

The version comes from `[workspace.package].version` in the root `Cargo.toml` — the same one-liner
`scripts/cut-release.sh`, `release.yml` and `crates-io.yml` read — so the image tag, the
`org.opencontainers.image.version` label and the Kustomize image tag all name one release.

`--binary PATH` builds from a binary you already have. It is for development and for the container
integration test, and it carries no release provenance; nothing built that way should be published.

**Not yet wired:** no workflow pushes this image to a registry. The GitHub Release asset inventory is
closed at 28 names and structurally enforced, so a published image belongs in a registry with its own
job rather than as a release asset. Until that job exists, the image is built from a release rather
than published with one.

## Checks

```sh
# Artifact contract — runs in ordinary CI, no Docker needed.
cargo test -p flux-cli --test deployment_artifacts

# The container profile end to end: build the image, mount a workspace/TLS/token, drive it with a
# client, restart it, prove the workspace and delivery ledger survived. Opt-in, because Docker is
# not available in ordinary workspace CI.
cargo build --target x86_64-unknown-linux-musl --bin flux
FLUX_TEST_CONTAINER=1 cargo test -p codewandler-flux-server --test remote_system_container

# The Kubernetes profile renders and validates against real API schemas.
kubectl kustomize deploy/kubernetes | kubectl apply --dry-run=client -f -

# The guest unit parses and its hardening is what it claims.
systemd-analyze verify deploy/vm/flux-system.service
systemd-analyze security --offline=true deploy/vm/flux-system.service
```
