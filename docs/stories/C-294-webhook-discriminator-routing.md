---
id: C-294
title: "Route a webhook to a trigger label by its event discriminator"
pillar: Core
status: backlog
epic: verified-webhook-channel
note: "today one webhook channel = one trigger label, so every vendor event lands in one journey that must switch on JSON — while the vendor already tells us the type in a header or a body field"
---

# Route a webhook to a trigger label by its event discriminator

## Goal

Let a webhook channel fan out to per-event triggers — `trigger on "github.issues.opened"` — instead of
a single trigger that reimplements dispatch inside the flow.

## Context — verified against this tree

- `handle` delivers under the **channel's own name** and nothing else:
  `state.deliverer.deliver(&state.name, body)` — `crates/flux-channels/src/adapters/webhook.rs:110`,
  and `:103` in the async branch. One channel, one label.
- `Event` is `{ label, payload }` — `crates/flux-app/src/bus.rs:115-118` — so the label is the only
  routing key that exists.

## Acceptance

- [ ] An optional `discriminator` on the channel: `source` (`header` | `body`), `name`, and an optional
      `when` narrowing on a body field (GitHub sends one `issues` event with an `action` field, and
      `{ action = "opened" }` is what makes `issues.opened` a distinct thing a trigger can match).
- [ ] The channel fires `"<channel>.<event>"` when the discriminator resolves, and plain `"<channel>"`
      when it does not — so **existing single-trigger programs keep working unchanged**. Test
      `channel_without_discriminator_still_fires_its_own_label`.
- [ ] **Failing-first test `discriminator_routes_to_distinct_triggers`**: two events on one channel
      reach two different triggers.
- [ ] **The discriminator is read after verification, from the decoded body or the headers** — never
      before. A `source = "body"` discriminator is legitimate here precisely because routing happens
      downstream of C-291's decode; that is the difference from the verification timestamp, which must
      be header-borne. Assert the ordering: a request with a bad signature is rejected before its
      discriminator is looked at, and delivers to no label at all.
- [ ] An event with **no matching trigger is not an error** — it is a logged no-op. Vendors send event
      types nobody subscribed to, and a 500 on one teaches the vendor to retry it forever.
- [ ] Trigger matching stays an **exact label match**. No globbing is introduced by this story;
      `trigger on "gh.*"` is a separate decision with its own precedence questions.
- [ ] A discriminator value that would produce a label with unexpected characters is sanitised or
      refused — the value comes from the vendor's request, and it becomes a routing key.

## Progress

- (not started)

## Notes

- Depends on **C-291** (raw-body capture and the `verify` declaration) for the ordering guarantee.
  Independent of C-292 and C-293.
- Design: `../flux-connectors/docs/designs/verified-webhook-seam.md` §1 (step 7) and §5, capability 4.
- The upstream declaration is flux-connectors' `ChannelBinding::discriminator`, a
  `Selector { source, name }` over `FieldSource::Header | Body`
  (`crates/connector-spec/src/inbound.rs:100-105`, `:70-75`), and per-event narrowing lives in
  `EventDecl::when`. A connector generates the pair; flux consumes it.
- The event names an event keeps are the **vendor's own** (`github.issues.opened`, `app_mention`,
  `issues.opened`). There is deliberately no normalized cross-vendor event taxonomy on either side of
  this seam.
