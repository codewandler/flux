---
id: A-70
title: Parallelize installed-plugin startup handshakes
pillar: Agent
status: done
note: "C-54 control: normal HOME mock startup ~2.47s versus HOME=/tmp ~0.215s; plugin processes were loaded sequentially even though their handshakes are independent."
---

# Parallelize installed-plugin startup handshakes

## Goal

Remove seconds of fixed per-command latency from users with many installed plugins without changing
which verified plugins load, their guarded host capabilities, operation schemas, or deterministic
catalog order.

## Acceptance

- [x] Independent plugin manifest/schema handshakes run concurrently with a bounded fan-out,
      including their synchronous verification/spawn prefix.
- [x] Results are sorted by plugin name before registry/group insertion, preserving cache-stable
      prompt and merge order.
- [x] Every child process still starts through `System`; manifest-scoped deny-by-default host
      capabilities and verification are unchanged.
- [x] A concurrency regression test proves at least two independent loads overlap.
- [x] Normal-HOME mock and live-provider startup traces record before/after wall time and provider
      request start gap.
