---
id: C-729
title: "The crate-version guard compares against the registry, not just git"
pillar: "Core"
status: backlog
priority: 1
epic: delivery-is-verified
areas: [flux-release]
note: "codewandler-flux-secret 1.3.0 was published during a failed release run, then C-709 added a pub host field to that same version. check-crate-versions.sh compares a crate's version against git history, so the bump from 1.2.0 to 1.3.0 satisfied it and the later content change did not. The result was a green CI and a crates.io publish that stopped at flux-capabilities with 'no field host on type &EndpointRef', because the published 1.3.0 and the local 1.3.0 are different crates. The guard must ask crates.io whether this exact version is already published and whether its content still matches"
---

# The crate-version guard compares against the registry, not just git

## Goal


## Acceptance

- [ ] Define acceptance.
