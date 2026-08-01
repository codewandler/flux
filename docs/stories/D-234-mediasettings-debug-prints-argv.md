---
id: D-234
title: "`MediaSettings` derives `Debug` and prints the full sidecar argv — where a resolved secret can live"
pillar: Agent
status: ready
priority: 4
design: docs/designs/meeting-rooms.md
epic: meeting-rooms
areas: [flux-channels]
note: "found by D-208's review. The downstream MediaSidecarConfig redaction is correct; the layer ABOVE is not. flux-app's resolve_in recurses into arrays, so `sidecar [\"chrome\", secret \"ROOM_TOKEN\"]` is host-resolved plaintext in argv[1] — and the preflight runbook actively teaches operators to put host specifics there"
---

# The redaction is one layer too low

## Goal

Stop `MediaSettings` offering a resolved secret to a formatter, matching the precedent the crate
already states and tests.

## The finding

`crates/flux-channels/src/config.rs` — `#[derive(Debug, Clone, Deserialize)] pub struct MediaSettings {
pub sidecar: Vec<String>, … }`, reachable through `RoomSettings`'s own derive.

`flux-app`'s `resolve_in` recurses into `Value::Array` (`crates/flux-app/src/secrets.rs:60-67`), so
`sidecar ["chrome", secret "ROOM_TOKEN"]` becomes **host-resolved plaintext** in `sidecar[1]`. And this
is not hypothetical: D-208's own preflight runbook and `tests/room_media.rs` actively teach operators to
put host specifics in argv.

The crate already states the rule this violates — *"the type itself must not offer the value to a
formatter"* (`config.rs:19-25`) — and already tests it for the webhook case
(`adapters::webhook::tests::the_settings_debug_prints_neither_the_token_nor_the_signing_secret`).

⚠ D-208's `MediaSidecarConfig`/`SidecarMediaPeer` **do** redact argv past `argv[0]`; the gap is the
config layer above them.

## Acceptance

- [ ] **Failing-first**: a test asserting `MediaSettings`' (and `RoomSettings`') `Debug` prints neither a
      resolved secret nor argv past `argv[0]` — failing at the merge base.
- [ ] Follow `WebhookSettings`' hand-written `Debug` precedent exactly rather than inventing a second
      convention.
- [ ] ⚠ Check the neighbours while here: `ChannelDecl` (`crates/flux-lang/src/program.rs:75`) already
      derives `Debug` over the same resolved `Value`, so the exposure class **pre-exists at the decl
      layer**. Either fix it too or record explicitly why it is out of scope — it is the reason this was
      judged non-blocking, not a reason it is fine.

## Notes

- Not blocking and not a demonstrated leak: nothing in D-208's diff formats `RoomSettings`, and the
  redactor *is* seeded with the value (`secrets.rs:44-51`), so a leak through a redacted sink is
  covered. This closes the unredacted-sink path.
- Same family as [C-432](C-432-browser-credentials-never-come-from-the-prompt.md) and
  [C-339](C-339-redaction-falls-back-to-the-unredacted-value.md): the `Redactor` is not the only defence,
  and a type that hands a formatter a secret is the one place it cannot help.

## Progress

- Filed 2026-08-02 from D-208's review.
