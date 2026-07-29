---
id: A-124
title: DockerRuntime — an agent as a container
pillar: Agent
status: backlog
epic: agent-fleet-runtime
design: docs/designs/agent-fleet-runtime.md
areas: [flux-fleet]
note: "flux ships no Dockerfile today — this story owns the image contract as well as the runtime"
---

# DockerRuntime — an agent as a container

## Goal
`docker://ghcr.io/acme/worker:1.2` starts a container serving A2A and returns its address. The
container is the unit of isolation the process runtime cannot give: its own filesystem, its own
resource limits, its own failure domain.

## Acceptance
- [ ] `DockerRuntime` implements `AgentRuntime` and **passes A-121's contract suite unmodified**.
- [ ] Failing-first test: `status` distinguishes *scheduled* from *ready* — a container that is
      running but whose agent card does not yet answer reports `Starting`, not `Ready`.
- [ ] Failing-first test: `stop` removes the container and reports `Exited` with the container's
      exit code; a second `stop` is idempotent.
- [ ] The daemon socket is declared as concrete authority via `access()` — access to the docker
      socket is root-equivalent on most hosts, and it must be policy-visible rather than assumed.
- [ ] Tests run **offline against a stubbed daemon API**; a real-docker test may exist but must be
      ignored by default, per the repo's offline-first rule.
- [ ] An agent image contract is documented (what the image must expose: the A2A port, a persistent
      store path) — flux currently ships no Dockerfile at all, so this story defines the shape rather
      than inheriting one.

## Progress
- (not started)

## Notes
- Design: [agent-fleet-runtime.md](../designs/agent-fleet-runtime.md).
- Depends on A-120, A-121. Independent of A-125 — they can run in parallel.
- Whether the docker API is reached directly or through a provider plugin is an implementation
  choice; if a plugin, follow `plugins/AUTHORING.md` and the endpoint-provider pattern.
