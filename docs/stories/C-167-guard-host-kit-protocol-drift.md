---
id: C-167
title: Fail loudly when host-kit lags the protocol version it is built against
pillar: Core
status: backlog
priority:
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
- (not started)

## Notes
- Surfaced while shipping v0.29.0: `host-kit@1.0.0` reached crates.io only via the hand-dispatched
  `plugins-v0.1.2` release, after the flux release had already told users to depend on it.
- Cheapest form is probably a crates.io API comparison in the same shape as
  `scripts/check-crate-versions.sh`, reusing its version-reading helpers.
- Related: [C-166](C-166-pin-an-old-pack-for-the-compat-test.md).
