---
id: A-124
title: DockerRuntime — an agent as a container
pillar: Agent
status: ready
priority: 10
epic: agent-fleet-runtime
design: docs/designs/agent-fleet-runtime.md
areas: [flux-runtime, flux-orchestrate, plugins]
note: "flux ships no Dockerfile today — this story owns the image contract as well as the runtime"
---

# DockerRuntime — an agent as a container

## Goal
A `DockerRuntime` configured with an agent image starts a container serving A2A and returns the
opaque worker id and endpoint C-243's `AgentRuntime` requires. The container is the unit of
isolation the process runtime cannot give: its own filesystem, resource limits, and failure domain.
The image/configuration selects the backend; this story does not revive the superseded
runtime-selecting `docker://` URI.

## Acceptance
- [ ] `DockerRuntime` implements the shipped `flux_runtime::AgentRuntime` contract and shares a
      backend contract suite with `ProcessRuntime` rather than inventing Docker-only lifecycle rules.
- [ ] Failing-first test: `status` distinguishes *scheduled* from *ready* — a container that is
      running but whose agent card does not yet answer reports `WorkerState::Starting`, not `Live`.
- [ ] Failing-first test: `stop` removes the container and reports its exit code through
      `WorkerStatus`; a second `stop` is idempotent.
- [ ] Docker access remains concrete, policy-visible host authority. Reuse the checked-in Docker
      plugin/capability boundary or declare an equally narrow guarded process contract; do not add a
      direct socket path or an `access()` method that the shipped `AgentRuntime` trait does not have.
- [ ] Tests run **offline against a stubbed daemon API**; a real-docker test may exist but must be
      ignored by default, per the repo's offline-first rule.
- [ ] An agent image contract is documented (what the image must expose: the A2A port, a persistent
      store path) — flux currently ships no Dockerfile at all, so this story defines the shape rather
      than inheriting one.

## Progress
- (not started)

## Notes
- Design: [agent-fleet-runtime.md](../designs/agent-fleet-runtime.md).
- The prerequisite port/process implementation shipped in C-243. Independent of A-125 — they can
  run in parallel.
- Whether the docker API is reached directly or through a provider plugin is an implementation
  choice; if a plugin, follow `plugins/AUTHORING.md` and the endpoint-provider pattern.
