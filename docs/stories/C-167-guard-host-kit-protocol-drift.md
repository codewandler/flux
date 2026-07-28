---
id: C-167
title: Fail loudly when host-kit lags the protocol version it is built against
pillar: Core
status: done
epic: plugin-protocol-decoupling
design: docs/designs/plugin-protocol-decoupling.md
note: host-kit publishes only from a pack release, so a wire change can ship flux-plugin-protocol to crates.io while host-kit silently stays behind — the drift the old lockstep prevented by construction
---

# Fail loudly when host-kit lags the protocol version it is built against

## Goal

Close the one guarantee the version lockstep provided that nothing has replaced: that the plugin
SDK on crates.io is never older than the wire contract it is supposed to speak.

## Why (evidence)

C-146 moved `codewandler-flux-host-kit` out of the flux closure and into the pack release, because a
flux cut cannot change its version. The consequence showed up immediately at v0.29.0: the release
notes told plugin authors to depend on `codewandler-flux-host-kit = "1"`, `flux-plugin-protocol@1.0.0`
went live with the flux closure, and host-kit sat at **0.28.0** on crates.io until a pack release was
cut by hand afterwards. For that window the published advice was unfollowable.

The existing checks do not cover this:

- `release-plugins.yml`'s preflight checks *ordering* — it refuses to publish host-kit before its
  protocol dependency is live. It cannot fire at all if nobody runs the pack workflow.
- `scripts/check-crate-versions.sh` checks that a **changed** crate moved its version. host-kit not
  being published is not a change.
- `publish_script_covers_a_registry_resolvable_closure` checks that some publisher *names* host-kit,
  not that it ever ran.

So the failure mode is an omission, and every current guard watches for commissions.

## Acceptance

- [ ] A check fails when the `codewandler-flux-plugin-protocol` version live on crates.io is newer
      than the protocol version the published `codewandler-flux-host-kit` depends on.
- [ ] It runs where it can actually block: as part of the flux release path (so cutting flux tells
      you a pack release is now owed), not only inside the pack workflow that may never be run.
- [ ] The message says what to do — run `release-plugins.yml` with `publish: true` at pack version
      X — rather than merely reporting a mismatch.
- [ ] Failing-first: a fixture where host-kit's published protocol requirement is behind the live
      protocol version must fail the check.
- [ ] AGENTS.md's protocol-line section records that a wire change implies a pack release, and that
      this check enforces it.

## Progress
- 2026-07-28: Implemented.
  - New `scripts/check-host-kit-protocol-drift.sh`: live comparison between the
    `codewandler-flux-plugin-protocol` version live on crates.io and the protocol dependency
    requirement recorded by the currently-published `codewandler-flux-host-kit` (crates.io API:
    `/crates/<name>` for `max_stable_version`, `/crates/<name>/<version>/dependencies` for the
    `req` on `codewandler-flux-plugin-protocol`, parsed with `python3 -c "import json..."` —
    same style already used by `scripts/publish-crates-io.sh`). An absent dependency (the exact
    pre-split v0.29.0 incident shape — host-kit didn't even reference the not-yet-split protocol
    crate) is treated as "requires nothing", i.e. maximally stale, not a special-cased pass. On
    drift, the failure message names the concrete next step: `run .github/workflows/release-plugins.yml
    with publish: true at pack version <plugins/Cargo.toml's workspace.package.version>`.
    Requires the descriptive `User-Agent` header crates.io's data-access policy demands (plain
    `curl` 403s without one — confirmed live; reused the same UA string `publish-crates-io.sh` and
    `release-plugins.yml` already send).
  - `--self-test` mode (offline, mirrors `check-crate-versions.sh`'s pattern): proves (a) a stale
    requirement (`^1.0.0` vs. a live `1.1.0`) is flagged with the actionable message, (b) an absent
    dependency (the real incident shape) is flagged, (c) a requirement that already covers the
    live version passes. This is the failing-first proof; ran green.
  - Wired into `.github/workflows/ci.yml` as new job `host-kit-protocol-drift`, parallel to
    `crate-versions`/`plugin-compat` — runs `--self-test` then the live check on every push to
    main/PR, i.e. inside the flux release path itself (every release commit lands via a push to
    main), not only inside `release-plugins.yml` which may never be dispatched. Exit 2 (crates.io
    state unobtainable) is a logged `::warning::` skip, never conflated with a real drift (exit 1).
  - `AGENTS.md`'s protocol-line section (the "one documented exception to the single-version rule"
    paragraph) now records that a wire change implies a pack release is owed and names this script
    + CI job as the enforcement.
  - `docs/designs/plugin-protocol-decoupling.md`'s "As built" table gained a row for this guard.
  - Verified against real, live crates.io state (this sandbox has network access to crates.io once
    the required `User-Agent` header is set): `codewandler-flux-plugin-protocol` live = `1.0.0`;
    `codewandler-flux-host-kit` live = `1.0.0`, published dependency req on the protocol crate =
    `^1` → **PASS** (`'^1' -> 1.0.0` covers live `1.0.0`) — the v0.29.0 incident is already
    resolved in the current live state, so no drift is reported, as expected.
  - Gate: no Rust crate changed by this story (a new script + CI + docs). `bash -n` on the new
    script; `.github/workflows/ci.yml` validated with `python3 -c "import yaml; yaml.safe_load(...)"`;
    `--self-test` and the live check both run and shown above.

## Notes
- Surfaced while shipping v0.29.0: `host-kit@1.0.0` reached crates.io only via the hand-dispatched
  `plugins-v0.1.2` release, after the flux release had already told users to depend on it.
- Cheapest form is probably a crates.io API comparison in the same shape as
  `scripts/check-crate-versions.sh`, reusing its version-reading helpers.
- Related: [C-166](C-166-pin-an-old-pack-for-the-compat-test.md).
