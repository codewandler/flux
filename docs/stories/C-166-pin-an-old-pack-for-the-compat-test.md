---
id: C-166
title: Keep the plugin compat test an ACROSS-TIME test, not a same-generation one
pillar: Core
status: done
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
- 2026-07-28: Implemented.
  - `scripts/check-plugin-compat.sh`: added a guarantee-logging block right after resolving `$TAG`
    that downloads the release's `plugins-index.json` (best-effort) and logs the pack's release
    age and the protocol marker its binaries were built against, alongside this host's own marker
    (read from `crates/flux-plugin-protocol/src/lib.rs`) — e.g. `guarantee: pack released
    2026-07-10T13:38:29Z (18d ago), binaries built against protocol 'flux.plugin.v1' (this host:
    'flux.plugin.v1')`. Also corrected two stale comments/log lines discovered while verifying this
    story: `flux plugin list` does **not** round-trip the wire in the current CLI (verified live —
    it's a purely local, descriptor-based listing); the actual protocol check happens at `flux
    plugin call` (`PluginHost::spawn_verified` → `manifest()` → `check_protocol` in
    `crates/flux-plugin/src/host/loading.rs`). The comments now attribute the guarantee to the
    right step instead of overclaiming what `list` proves.
  - `.github/workflows/ci.yml`'s `plugin-compat` job now runs the script **twice**, as two
    separate, never-merged assertions: (1) pinned via job-level `COMPAT_BASELINE_TAG:
    plugins-v0.1.1` — verified this genuinely predates `crates/flux-plugin-protocol` existing as
    its own crate (`git merge-base --is-ancestor d19212f f1fda342...` fails, i.e. NOT an ancestor);
    (2) the newest pack via the script's existing default resolution (currently `plugins-v0.1.3`),
    unchanged behavior, kept as the separate "current pack still works" assertion.
  - `docs/designs/plugin-protocol-decoupling.md`'s "As built" table row for this guarantee now
    records both pack tags the guarantee rests on and what each run proves.
  - Failing-first, run for real (this repo has 3 real `plugins-v*` GitHub releases and live `gh`
    network access):
    - `PACK_TAG=plugins-v0.1.1 ./scripts/check-plugin-compat.sh` → **PASS**, log line: `guarantee:
      pack released 2026-07-10T13:38:29Z (18d ago), binaries built against protocol
      'flux.plugin.v1' (this host: 'flux.plugin.v1')` — proves "a pack whose binaries predate the
      protocol crate must still pass".
    - `./scripts/check-plugin-compat.sh` (default → `plugins-v0.1.3`, released today) → **PASS**,
      separate log line confirming it's the secondary/current-pack assertion.
    - Synthetic marker mismatch: built the existing `future_protocol_plugin` fixture
      (`crates/flux-plugin/src/bin/future_protocol_plugin.rs`), disguised it as
      `flux-plugin-fakemismatch`, installed it into a throwaway `HOME` via `flux plugin install
      --dir=`, then ran `flux plugin call fakemismatch sources '{}'` against the real `flux`
      binary → exit 1, message `plugin speaks protocol \`flux.plugin.v99\`, this host speaks
      \`flux.plugin.v1\` — upgrade whichever side is older …`. Confirmed the script's grep pattern
      (`speaks protocol|unsupported protocol|invalid frame`) matches this real text, i.e. the FAIL
      path genuinely fires.
  - Gate: no Rust crate changed by this story (scripts/CI/docs only). `bash -n` on the modified
    script; `.github/workflows/ci.yml` validated with `python3 -c "import yaml; yaml.safe_load(...)"`;
    the full script exercised live end-to-end against real GitHub releases as above.

## Notes
- Surfaced while shipping v0.29.0/v0.30.0 — see the CHANGELOG entries for C-145.
- `scripts/check-plugin-compat.sh` already honours `PACK_TAG`; this is mostly about *choosing* the
  tag deliberately and saying so in the log.
- Related: [C-167](C-167-guard-host-kit-protocol-drift.md), the other gap the split introduced.
