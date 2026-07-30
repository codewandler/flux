---
id: D-219
title: "Who may trigger this agent stays operator config — allow-lists keyed on the binding's payload symbols"
pillar: Agent
status: backlog
epic: connector-channels
note: "allow_users/allow_channels are a DEPLOYMENT policy about who may wake an agent; a vendor spec cannot know them and a connector upgrade must not be able to change them. Generalise them without moving them into the manifest — the keys are payload symbols the binding declares, the values are this deployment's ids"
---

# Who may trigger this agent stays operator config — allow-lists keyed on the binding's payload symbols

## Goal

Keep the Slack adapter's allow-list behaviour, generically and on the **flux** side, so a connector
channel can restrict who may wake an agent without any of that policy passing through a published
vendor manifest.

## Context — verified against this tree

- `SlackSettings` carries `allow_users` and `allow_channels`
  (`crates/flux-channels/src/config.rs:98-103`), gated at
  `crates/flux-channels/src/adapters/slack.rs:131` via `allowed(…)` at `:183-187`. An empty list allows
  everyone; otherwise the id must be present.
- The two lists are the only vendor-agnostic thing in that file, and they are the one thing that must
  **not** become connector data: a vendor spec cannot know a deployment's user ids, and a connector
  upgrade must not be able to widen them.
- The generic key already exists on the connector side: `ChannelBinding::payload` is a map of Flux symbol
  → dotted path (`../flux-connectors/crates/connector-spec/src/inbound.rs:351`), and Slack declares
  `user` and `channel` among them (`../flux-connectors/providers/slack.toml:441-446`).

## Acceptance

- [ ] The channel declaration takes an `allow` record whose **keys are payload symbols the binding
      declares** and whose values are lists of permitted ids:

      ```flux
      allow { user = ["U0123ABCDEF"], channel = ["C0123ABCDEF"] }
      ```

- [ ] **Failing-first test `allow_key_that_is_not_a_declared_payload_symbol_is_a_load_error`.** A typo
      (`allow { users = [...] }`) must not construct a channel. This is the whole safety value of the
      story: a misspelled key that is silently ignored is an allow-list that allows everyone while
      appearing to restrict.
- [ ] The filter runs **after** verification and **before** delivery: a rejected event delivers nothing.
      Test asserts the recording deliverer's count is `0`, not the response status.
- [ ] An empty or absent list allows everything, matching `slack.rs:183-187` exactly. Document it as a
      permissive default rather than leaving it implied — an operator exposing a non-loopback endpoint
      should be told that verification proves *the vendor* sent it, not that *this user* may wake an agent.
- [ ] A symbol whose resolved value is absent from a particular event is treated as **not matching** a
      non-empty list, never as matching. Test `absent_symbol_does_not_satisfy_a_non_empty_allow_list`.
- [ ] Vendor-neutral by construction: a second fixture binding declaring `sender` and `repo` gets
      `allow { sender = [...], repo = [...] }` working with **zero** new code. Assert it.
- [ ] Nothing about the allow-list reaches the manifest, the catalogue, or any artifact. It exists only
      in the operator's program.

## Progress

- (not started)

## Notes

- Parent: **D-215**. Depends on **D-216**. Design:
  `../flux-connectors/docs/designs/connector-channel-seam.md`, section "Allow-lists are operator config,
  deliberately".
- **Rejected alternative — a `[channels.identity]` block in the manifest** declaring which payload
  symbols are identity-bearing. It buys a settings UI the ability to render "Allowed users" instead of
  "user", and it costs new upstream IR, a new refusal, and a new way for a published manifest to
  influence an access decision. Revisit when a UI needs it, not before.
- **Rejected alternative — keep `allow_users`/`allow_channels` as fixed field names.** They are Slack's
  vocabulary. A GitHub binding has senders and repositories, and hard-coding two Slack nouns is how the
  generic arm grows a vendor's shape back into it.
- This story is deliberately separate from D-216: an access-control filter that arrives with fifteen
  other refusals is an access-control filter nobody reviewed.
