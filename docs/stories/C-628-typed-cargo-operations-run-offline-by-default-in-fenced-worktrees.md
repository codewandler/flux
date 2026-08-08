---
id: C-628
title: "Typed cargo operations run offline by default in fenced worktrees"
pillar: "Core"
status: backlog
epic: fleet-harness-throughput
areas: [flux-tools]
note: "workers read crates.io DNS failures as test failures; CARGO_NET_OFFLINE cannot reach workers through SAFE_ENV; the exchange gate itself starts with cargo fetch --locked"
---

# Typed cargo operations run offline by default in fenced worktrees

## Goal

A fenced story worktree has no network, but typed cargo operations run as if it did: workers
read `Could not resolve host: index.crates.io` as their test failing, and `CARGO_NET_OFFLINE`
cannot reach a worker because SAFE_ENV strips it. The gate side has the same defect from the other
direction: the exchange repository gate begins with `cargo fetch --locked`, a network operation,
inside an offline sandbox.

## Acceptance

- [ ] cargo_check/build/test/clippy/fmt run with --offline (or an equivalent enforced env) by default inside fleet worktrees; a registry-resolution failure surfaces as an environment error, distinct from a test failure.
- [ ] A documented path exists for pre-warming or vendoring the registry (CARGO_HOME) host-side, and dispatch preparation uses it.
- [ ] Gate commands that need the network are executed or prepared host-side before the sandbox, or refused at validate time with a clear message.
