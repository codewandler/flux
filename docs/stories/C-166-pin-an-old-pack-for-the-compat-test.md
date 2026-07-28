---
id: C-166
title: Keep the plugin compat test an ACROSS-TIME test, not a same-generation one
pillar: Core
status: backlog
priority:
epic: plugin-protocol-decoupling
design: docs/designs/plugin-protocol-decoupling.md
note: check-plugin-compat.sh resolves the LATEST plugins-v* release, so a fresh pack release silently downgrades it from "yesterday's binary still works" to "today's binary works"
---

# Keep the plugin compat test an ACROSS-TIME test, not a same-generation one

## Goal

`scripts/check-plugin-compat.sh` is the test that backs the entire decoupling claim: a plugin built
against protocol 1.0 still speaks to a much later flux. Keep it testing that, rather than quietly
becoming a test that the pack we just built works with the host we just built.

## Why (evidence)

The script resolves the newest `plugins-v*` release. That was a genuine cross-time check at
v0.29.0, where it pulled **plugins-v0.1.1** — binaries compiled *before* the wire contract moved
into its own crate, against the old `flux-plugin` guest feature. Twenty minutes later the pack was
re-released as **0.1.2**, and the very next run (v0.30.0) tested binaries built from the same tree
as the host. Both runs are green and both report `PASS`, but they prove different things, and
nothing in the output distinguishes them — the log line reads `pack release: plugins-v0.1.2` either
way.

Left alone, the guard's strength is an accident of how recently someone cut a pack.

## Acceptance

- [ ] The job tests against a deliberately OLD pack, not merely the newest one — e.g. a
      `COMPAT_BASELINE_TAG` pinned in the workflow (or the oldest pack still speaking the current
      `PROTOCOL` marker), using the `PACK_TAG` override the script already supports.
- [ ] The log states which guarantee the run actually established: the age of the pack under test
      and the protocol marker its binaries were built against.
- [ ] Testing the newest pack is kept as a *second*, separate assertion — it is still worth knowing
      the current pack works — so the two are never conflated.
- [ ] Failing-first: pointing the job at a pack whose binaries predate the protocol crate must still
      pass; a synthetic marker mismatch must still fail.
- [ ] The design doc's "As built" table records which pack the guarantee rests on.

## Progress
- (not started)

## Notes
- Surfaced while shipping v0.29.0/v0.30.0 — see the CHANGELOG entries for C-145.
- `scripts/check-plugin-compat.sh` already honours `PACK_TAG`; this is mostly about *choosing* the
  tag deliberately and saying so in the log.
- Related: [C-167](C-167-guard-host-kit-protocol-drift.md), the other gap the split introduced.
