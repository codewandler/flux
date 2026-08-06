---
id: C-627
title: "Admission refuses a loop revision whose source digest changed"
pillar: "Core"
status: backlog
epic: fleet-harness-throughput
areas: [flux-cli]
note: "three different programs were served as implementation@9; revisions 3 and 5 share one digest; resume reconstruction failed for wave-338/340/350/360"
---

# Admission refuses a loop revision whose source digest changed

## Goal

Admission snapshots a loop's profile, revision and SHA-256 digest (decision 0014), but nothing
stops the source from changing under a pinned revision: three different programs were served as
`implementation@9`, and revisions 3 and 5 share one digest. Resume must reconstruct the admitted
binding; when the file at that revision no longer matches, reconstruction fails obscurely —
wave-338/340/350/360 all died in that cluster.

## Acceptance

- [ ] Admitting a loop whose profile revision was previously recorded with a different source digest is refused with a message naming both digests and the fix (bump the revision).
- [ ] `fleet validate` reports the same drift without spawning anything.
- [ ] A regression test pins the refusal and the happy path (bumped revision admits cleanly).
