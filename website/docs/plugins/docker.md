---
title: Docker plugin
description: "Install and use the Docker Engine plugin, understand its Unix-socket authority, and distinguish Docker management from execution placement."
---

# Docker plugin

The `docker` plugin lets Flux manage Docker Engine resources that already exist: containers, images,
networks, volumes, and daemon disk usage. It talks HTTP/1.1 to Docker Engine API v1.43 through the
host's guarded Unix-socket connection capability. The plugin process never opens the socket itself.

:::important This plugin manages Docker; it is not a Docker runtime
Installing the plugin does **not place** ordinary guarded effects inside containers and does not make
fleet workers run as containers. Those are separate proposed backends: C-397 for per-effect process
placement and A-124 for whole-agent worker placement. In agent `--remote` mode, plugins are currently
hidden and refused rather than run on the local machine as a fallback.
:::

## 1. Install

```bash
flux plugin install docker
flux plugin status docker
```

The default endpoint is `/var/run/docker.sock`. The live manifest should report the Docker
container/image/network/volume datasources and a guarded connection capability for the socket. The
plugin declares no Docker credential: access is whatever the operating-system permissions on that
socket grant to the user running Flux.

## 2. Treat the socket as host authority

Access to a Docker daemon is normally **root-equivalent** on that daemon's host. A caller that can
create a privileged container or mount `/` can escape the container abstraction and alter the host.
Flux therefore exposes the socket connection in the plugin's declared capabilities and keeps every
write behind the ordinary authorization and approval envelope; it is not ambient plugin IO.

The checked-in plugin allows `/var/run/docker.sock` and socket overrides matching
`/var/run/*.sock`. A per-call override is explicit:

```bash
flux plugin call docker docker.info '{"socket":"/var/run/docker.sock"}'
```

Do not forward an unauthenticated Docker TCP socket onto a network. This plugin's released contract
is a local Unix socket; Docker contexts, TLS client authentication, and SSH transports are not
silently inferred from your `docker` CLI configuration.

## 3. Verify the daemon

```bash
flux plugin call docker docker.info
flux plugin call docker docker.system.df
```

`docker.info` is the cheapest end-to-end check. A permission error means the current user cannot
dial the socket; a missing-file error means Docker is not listening at that path. Neither is fixed by
granting private-network egress because this is a Unix socket, not an HTTP URL.

## 4. Inspect resources

The read surface covers bounded, non-streaming inspection:

```bash
flux plugin call docker docker.container.list '{"all":true,"limit":20}'
flux plugin call docker docker.container.show '{"id":"api"}'
flux plugin call docker docker.container.logs '{"id":"api","tail":200}'
flux plugin call docker docker.container.top '{"id":"api"}'
flux plugin call docker docker.image.list
flux plugin call docker docker.network.list
flux plugin call docker docker.volume.list
```

Each resource family also has a raw inspect operation when the normalized projection omits a Docker
field you need: `docker.container.inspect.raw`, `docker.image.inspect.raw`,
`docker.network.inspect.raw`, and `docker.volume.inspect.raw`.

The plugin deliberately does not implement streaming/hijacked operations such as interactive exec,
live stats, followed logs, image build/push progress, or the daemon event stream. Those need a
long-lived or upgraded connection contract the current one-shot plugin operation surface does not
provide.

## 5. Mutate resources

Container lifecycle and object management are available as guarded writes:

```bash
flux plugin call docker docker.container.run \
  '{"image":"alpine:3.20","name":"flux-example","cmd":["sleep","60"]}' --dry-run
flux plugin call docker docker.container.start '{"id":"flux-example"}'
flux plugin call docker docker.container.stop '{"id":"flux-example","timeout":10}'
flux plugin call docker docker.image.pull '{"reference":"alpine:3.20"}'
flux plugin call docker docker.network.create '{"name":"flux-example"}' --dry-run
flux plugin call docker docker.volume.create '{"name":"flux-example"}' --dry-run
```

`--dry-run` validates the selected operation's schema without invoking it. Start, stop, restart,
create, run, pull, tag, and object creation are medium risk. Removes and every `*.prune` operation
are destructive. When an agent calls them, their declared effects and subjects pass through policy
and approval before the host lets the plugin dial Docker.

## Docker and remote mode

There are three different ways to involve a remote Docker host:

| Goal | Supported path today |
|---|---|
| Manage Docker resources | Run Flux and this plugin where the guarded Unix socket is available. |
| Land core guarded effects inside a container | Run [`flux system serve`](../remote-system-deployment.md) inside an operator-supplied container and select it with `--remote`. |
| Start a fleet worker as a Docker container | Proposed `DockerRuntime` (A-124); not shipped. |

The second row does not make the Docker plugin remote-capable. With `flux tui --remote`, native
integrations/plugins are omitted from the catalog because their host callbacks are not yet routed to
the selected execution system. Flux refuses that combination instead of pretending a remote Docker
operation ran while actually touching the local socket.

## Related docs

- [Using plugins](./using-plugins.md) — install, verify, pin, and remove plugins.
- [Execution topologies](../topologies.md#execution-placement-matrix) — management, effect placement,
  worker placement, and provisioning as separate jobs.
- [Deploy a remote execution system](../remote-system-deployment.md) — BYO container, pod, and
  microVM profiles.
- [Plugin capability sandbox](../security/plugin-sandbox.md) — host-enforced connection grants.
