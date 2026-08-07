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
- C-673 — harden the native reader against hostile mounts and pinned roots

## What a bound owes the answer (C-673)

Bounding a reading is not the same as reporting one honestly. Two rules the caps have to keep, and
neither is implied by "the list is capped":

- **A cap that drops something says so.** `DiskUsage::omitted_mounts` counts the mounts a reading
  does not carry, so a container host with a hundred filesystems cannot decode into the same answer
  as a machine that genuinely has thirty-two. It is the `Unavailable`-not-zero rule one level down.
- **A truncated identity stays distinct.** A mount point is a path, and paths agree for a long time
  before they differ, so cutting one at the instrument-label bound makes sibling containers report
  under a single name. `bounded_mount_point` spends the tail of the budget on a digest of the whole
  path instead of on a prefix the sibling also has.

Everything the reader collects is bounded while it is being collected rather than after — a hostile
`/proc/mounts` or `class/hwmon` must not be allocated in full first — and every parse answers
`ReadFailed` rather than panicking, including a `/proc/uptime` too large for a `Duration`.
