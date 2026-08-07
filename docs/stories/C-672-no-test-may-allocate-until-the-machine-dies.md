---
id: C-672
title: "No test may allocate until the machine dies"
pillar: "Core"
status: backlog
priority: 18
areas: [flux-tui]
note: "a flux-tui test binary reached 42 GB RSS / 75 GB virtual and triggered a global OOM that killed unrelated services"
---

# No test may allocate until the machine dies

## Goal

Bound a test that can take down everything else running on the host.

On 2026-08-07 at 03:11:20 the kernel OOM-killer fired globally. The process it killed was
`flux_tui-709cb1865a0f9255` — a **flux-tui test binary** — at **42 GB RSS and 75 GB virtual** on a
62 GB machine. Collateral damage included unrelated containerised services (nats, redis, coredns) that
have nothing to do with this repository.

This is not a slow test. A single test that can consume two thirds of the machine's memory invalidates
every concurrent build or gate on that host, and it does so in a way that looks like something else
failing. It is a plausible mechanism for the "load-dependent gate flake" that was investigated,
never reproduced, and closed as unreproducible — that occurrence was last seen with swap holding
45 GB, which is the same signature.

## Acceptance

- [ ] The allocating test is identified by name, and what it allocates is written down — a rendering
      test that grows with a dimension it does not bound is the likely shape.
- [ ] The allocation is bounded, and the bound is asserted in the test itself rather than left to the
      input that happens to be used.
- [ ] A regression test fails if the bound is removed — the failure mode is memory, so the guard has
      to be about memory, not about runtime.
- [ ] Any input dimension the renderer takes from data (width, height, body length, scroll extent) is
      clamped where it enters, so a large input degrades output rather than the host.
- [ ] The `P2` gate-flake note is revisited with this finding, since it was closed as "not
      reproducible" on the reasoning that memory pressure of that degree was not present. It was.

## Notes

- **Why this is not just a test bug.** The fleet's whole width argument assumes N workers can build
  and test concurrently on one machine. One unbounded test makes concurrency unsafe at any width, and
  makes every other measurement on this host suspect while it exists.
- The OOM record is in the kernel log; `journalctl -k` around that timestamp names the process, its
  RSS and the reaping. Start there rather than by guessing which test it was.
- Unrelated to the wave-472 recovery except in timing — that build survived the OOM and was later
  killed by a deliberate shutdown sweep. Do not conflate the two when reading the history.
