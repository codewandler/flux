---
id: C-414
title: "No fuzzing anywhere in the tree, on any parser or wire surface"
pillar: Core
status: ready
priority: 15
epic: security-assurance
areas: [ci, flux-lang, flux-plugin]
note: "F8 of the 2026-08-01 security-posture review at 0.47.1, LOW-MEDIUM. L-119 already scopes raw-text fuzzing for the flux-lang parser front-end; this story is the tree-wide question — the plugin NDJSON wire, the manifest deserialiser and the channel payload paths have no equivalent"
---

# Nothing in this tree is fuzzed

## Goal

Decide, once and deliberately, which untrusted-input surfaces get a fuzz lane — and get one lane
running.

The review found no fuzzing anywhere in the workspace. The surfaces that parse or deserialise
attacker-influenced bytes and have no fuzz coverage:

- the **flux-lang parser front-end** — already scoped by
  [L-119](L-119-raw-text-fuzzing.md), which is the narrower, filed version of this;
- the **plugin NDJSON wire and manifest deserialiser**, which take bytes from a plugin process;
- the **channel payload paths** (webhook, connector, room), which take bytes from the network.

⚠ This is an *assurance* gap, not a known defect. Nothing here says a bug exists; it says nothing
would find one of this class if it did. Rate it accordingly and do not let it displace the MEDIUM
findings from the same review.

## Acceptance

- [ ] The surfaces above are **triaged** — which get a fuzz lane, which do not, and why. A written
      "not worth it here" is an acceptable outcome; silence is not.
- [ ] At least one lane actually runs, on the highest-value surface the triage picks.
- [ ] The lane is wired somewhere it will be noticed. ⚠ A nightly-only lane blocks no push, PR or cut
      — L-118 is the story of a nightly lane whose preamble went on describing a fixed red for
      months because nothing put a reader in front of it. Decide where the signal lands.
- [ ] The relationship with L-119 is settled: this story absorbs it, or defers the parser lane to it
      and covers the rest. Do not implement the same lane twice.

## Notes

- Source: `docs/reviews/single/2026-08-01-security-posture-at-0.47.1.md`, F8.
- ⚠ L-121 records that rowan's green-node `Drop` is recursive and aborts around 4,000 levels — a fuzz
  harness that builds trees by hand can take the process down in *cleanup* and look like a crash in
  the code under test. Read it before writing a tree-building harness.

## Progress

- Filed 2026-08-01 from the 0.47.1 security-posture review.
