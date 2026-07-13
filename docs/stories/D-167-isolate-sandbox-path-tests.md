---
id: D-167
title: Isolate sandbox discovery tests from the process PATH
pillar: Agent
status: done
note: "2026-07-13 task install: 13 flux-system process tests failed together while PATH-mutating sandbox tests ran in the same parallel test binary."
---

# Isolate sandbox discovery tests from the process PATH

## Goal

Make `task install` reliable under Cargo's normal parallel test scheduler. Sandbox discovery tests
must validate PATH lookup without replacing the process-wide PATH seen by unrelated process tests.

## Acceptance

- [x] PATH lookup and bubblewrap discovery accept an injected search path in tests while production
      continues reading the caller's environment.
- [x] No `flux-system` test mutates process-wide `PATH`.
- [x] Tests prove injected lookup returns canonical absolute binaries and leaves the real PATH
      unchanged.
- [x] Repeated parallel `flux-system` runs pass, including every process test from the reported
      failure cluster.
- [x] The exact `task install` command completes successfully.
