---
id: C-653
title: "Typed host metrics vocabulary and the native backend"
pillar: "Core"
status: done
epic: host-metrics-seam
areas: [flux-system]
design: host-metrics-seam
note: "Decision 0018 rule 6: closed metric vocabulary, fail-closed Unserved, native reads the local machine; unsupported is explicitly unavailable, never zero"
---

# Typed host metrics vocabulary and the native backend

## Goal

A host exposes one bounded metrics read seam about its own substrate. A typed, closed metric
vocabulary — CPU usage and load, memory, swap, per-mount disk capacity/usage, uptime, temperature
sensors, fan speeds — joins the guarded port with a fail-closed `Unserved` default. The native
backend reads the local machine (procfs, sysfs hwmon, statvfs). An unsupported metric is
explicitly unavailable, never zero — the same convention the board statistics contract already
fixes for unsupported dimensions.

## Acceptance

- [x] A `GuardedMetrics` trait with a closed `MetricKind` vocabulary joins `ExecutionSystem`;
      defaults are `Unserved`; the codegate census entry is reviewed.
- [x] The native `System` serves cpu, memory, disk, load and uptime on Linux; temperature and fan
      serve where hwmon exposes them and answer explicitly-unavailable otherwise; tests cover both
      faces.
- [x] Readings are typed and unit-bearing, bounded in size, and carry a sampled-at timestamp; no
      free-form string metrics exist.
- [x] Dependency additions follow the existing `flux-system` policy; the parsing lives in
      `flux-system`, not in a consumer crate.
