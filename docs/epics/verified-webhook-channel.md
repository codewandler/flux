---
id: E-134
title: "The verified webhook channel — a delivery flux can prove came from who it claims"
tracker: C-419
---

# The verified webhook channel — a delivery flux can prove came from who it claims

## Why

Its history was written down in [C-419](../stories/C-419-verified-webhook-channel-epic.md), which stays the narrative record.

## Success criteria

- [ ] One signature implementation, parameterized — not one per vendor. Constant-time compare and a
      replay bound are properties of the shared implementation, not of each caller.
- [ ] `verified` is a fact the envelope carries, and nothing downstream infers it from arrival.
- [ ] A challenge/handshake is answered without waking an agent — an endpoint-verification GET must
      not cost a turn.
- [ ] Routing by event discriminator does not require the agent to parse the body to decide whether
      it cares.
- [ ] The relationship with C-409 and C-416 is stated in `docs/roadmap.md`, so the three are not
      implemented as three unrelated answers to one request path.

## Exit criteria

- [ ] Every story carrying `epic: verified-webhook-channel` is `done` (`flux board epics --slug verified-webhook-channel`).
- [ ] Every success criterion above is ticked.
