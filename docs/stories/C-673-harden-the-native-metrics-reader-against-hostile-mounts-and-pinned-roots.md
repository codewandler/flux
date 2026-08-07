---
id: C-673
title: "Harden the native metrics reader against hostile mounts and pinned roots"
pillar: "Core"
status: backlog
epic: host-metrics-seam
areas: [flux-system]
design: host-metrics-seam
note: "C-653 review findings: the reading is bounded at the answer, not at every seam edge; caps truncate silently and one parse path can panic"
---

# Harden the native metrics reader against hostile mounts and pinned roots

## Goal

C-653's review confirmed the metrics reader honest at the answer boundary but found the edges
softer than the module header claims. A finite-but-huge `/proc/uptime` value reaches
`Duration::from_secs_f64` and panics instead of answering `ReadFailed`, contradicting the module's
own "every failure here is an answer" contract (reachable through `MetricsRoots::pinned`, which the
docs support for non-standard kernel mounts). The mount listing drops entries past the cap with no
indicator and truncates mount points to the 64-byte instrument-label bound — deduplicating
*before* truncating, so two long sibling mounts collide into one reading; real overlay/docker
paths exceed that bound. The hwmon walk materializes the full listing before the cap bites. The
network-filesystem exclusion misses generic `fuse.*`, `9p`, `ceph`, `glusterfs` and `davfs`, which
still reach the synchronous `statvfs`. And the codegate allowance note overclaims ("a caller can
narrow but never widen the roots") where the true property is narrower: production entry points
always install `/proc`+`/sys` and nothing operation-, CLI- or wire-shaped reaches the setter.

## Acceptance

- [ ] A finite oversized uptime value answers `ReadFailed` rather than panicking; a test drives it
      through `MetricsRoots::pinned`.
- [ ] Exceeding the mount cap is observable in the answer, and mount points longer than the label
      bound keep distinct identities (no dedup-then-truncate collision); a test proves both with
      overlay-length paths, independent of `TMPDIR` length.
- [ ] The non-disk exclusion covers `fuse.*` generically plus `9p`, `ceph`, `glusterfs` and
      `davfs`, and the hwmon walk bounds its intermediate collection, not only its answer.
- [ ] The codegate allowance note states exactly the property the code enforces, and names the
      grep-provable fact that only tests reach the roots setter.
