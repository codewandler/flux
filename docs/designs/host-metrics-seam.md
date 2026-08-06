# Design — Host metrics seam

## Why

A host exposes nothing about itself. Dispatch-layer telemetry, substrate provenance and the usage
observatory all describe *work flowing through* a substrate, but there is no vocabulary for the
substrate's own condition — CPU, memory, disk, temperature, fan, cluster node capacity. Decision
0018 rule 6 fixes the seam: every host serves one bounded, typed metrics read surface about its
own substrate, and an unsupported metric is explicitly unavailable, never zero.

## Approach

A closed `MetricKind` vocabulary and a `GuardedMetrics` trait join the guarded port with
fail-closed `Unserved` defaults. Each backend serves what its substrate can honestly measure: the
native host reads procfs/sysfs, a remote host reports what its serving process measures (stamped
`remotely_reported`), a Kubernetes host maps node/pod readings into the same kinds. The wire
gains one bounded `host.metrics` operation under a protocol version bump, and the surface is
`flux host metrics <name>` plus an ambient-gated operation, projected into the existing
usage/monitoring views.

## Stories

- C-653 — typed host metrics vocabulary and the native backend
- C-654 — host metrics over the remote protocol and the host surface
- C-655 — a Kubernetes host serves node metrics
