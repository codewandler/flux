---
id: C-702
title: "A guaranteed-denied operation is not offered to the model"
pillar: "Core"
status: ready
priority: 3
epic: first-class-hosts
areas: [flux-runtime]
design: docs/designs/the-substrate-seam.md
note: "observed: plain flux in /tmp offers slack/web fetches that egress policy refuses unconditionally; the fleet hit the same class as 16 guaranteed denials and 5 dead datasource tools"
---

# A guaranteed-denied operation is not offered to the model

## Goal

Start flux in a directory with no configuration, ask it to fetch Slack messages, and it will
propose the fetch, spend a round on it, and be refused by egress policy — every time, because the
refusal was knowable before the model ever saw the tool. The same defect class was measured in the
fleet: sixteen guaranteed denials and five dead datasource tools burned worker rounds on calls that
could not have succeeded. `[tools] disable` already proves the mechanism exists — `resolve_disabled`
turns patterns into a concrete set of op names withheld from the catalogue — but nothing drives that
set from what policy would actually refuse.

The distinction that makes this safe is **guaranteed** versus **conditional**. An operation that
would *prompt* must still be offered: the human may approve it, and withholding it would silently
shrink what the agent can do. An operation that is refused unconditionally — a deny rule matching
it, an egress family with no reachable destination, a datasource op with no configured datasource,
a family the selected substrate answers `Unserved` for — is dead weight, and offering it trades a
round and a confusing refusal for nothing.

## Acceptance

- [ ] The catalogue offered to the model excludes operations whose refusal is unconditional under
      the session's resolved policy, permissions, egress posture, and selected substrate; anything
      that would merely prompt stays offered.
- [ ] Exclusion is computed once from session-stable state, matching how context assembly already
      works, and cannot vary per turn — a catalogue that changed mid-session would invalidate the
      cache-stable prompt prefix.
- [ ] Nothing is withheld silently from the operator: a surface (`flux tools --explain` or
      equivalent) lists what was withheld and names the rule that withheld it, so "why won't it
      fetch" is answerable without reading config by hand.
- [ ] A substrate that answers `Unserved` for a family withholds that family's operations for the
      session — a remote host with no HTTP wire should not offer `http.request` — and re-offering
      happens only through a new session, never mid-turn.
- [ ] Tests cover: a deny rule, an egress family with no admitted destination, a family the
      substrate does not serve, and the negative case that a prompt-only operation is still offered.

## Evidence — wave-602

The fleet half of this defect, measured exactly. The story-worker loop's `ai_segment` `tools:` list
names all six operations the datasource pack registers — `search`, `get`, `list`, `relation`,
`batch_get`, `sources` (`flux-capabilities/src/datasource/ops.rs`) — and every one of them requires
`datasource.read`. The story worker's `capabilities = ["read", "edit", "git", "rust", "node"]` grants
no such access, so all six are refusable before the model is ever asked.

Across the three surviving worker stores, `sources` was called and denied every time it was reached:

    `sources` denied by policy (datasource.read on Datasource)

7 attempts, 7 denials, 0 successes. It is the model's **step-3 gather call** on a fresh logical run,
so a worker that restarts a segment pays for it again — `wave-602-worker-1` restarted twice and
burned it three times. The other five were never reached only because `sources` fails first; nothing
distinguishes them.

Note the shape this takes in practice: the model is not being reckless. `sources` is a sensible first
move for "what can I read here?", it is in the catalogue it was handed, and the only way to discover
otherwise is to spend a round finding out. That is the whole argument of this story in one call.

Interim mitigation applied in the roadmap repository: the six operations were removed from
`story-implementation.flux`'s `tools:` list (loop profile revision 9 → 10). That fixes one authored
loop by hand. This story is what stops the next one from having the same hole.
