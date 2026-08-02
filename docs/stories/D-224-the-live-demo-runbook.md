---
id: D-224
title: "The live demo — flux is invited from Slack, joins the call, speaks, and answers in front of an audience"
pillar: Agent
status: ready
priority: 1
design: docs/designs/meeting-rooms.md
epic: meeting-rooms
areas: [flux-channels, flux-cli, docs]
note: "⚠ NOT new capability — the end-to-end scenario as one owned deliverable over D-206/D-208/D-209/D-210/D-211, which already exist. Filed because a demo assembled from five stories that each pass in isolation is exactly the thing that fails live. ⚠ Google Meet has NO path: the sidecar runs lib-jitsi-meet. Brave Talk / JaaS does, and a 2026-07-30 spike already got audible audio out"
---

# The demo, as a deliverable rather than a hope

## Goal

Run the whole scenario, in front of a real audience, without a rehearsal-only path:

1. Screen is shared. In Slack: `@flux <call link>`.
2. Slack shows *"flux is typing…"*, then flux answers in-channel — it acknowledges before it acts.
3. Everyone in the call sees **flux** asking to join. It is admitted.
4. *"Hey flux, how are you"* → flux starts a screenshare (black, ~2 s), then speaks:
   *"Good, thank you — hello everyone, I am flux."*
5. Live questions: *"what's the current bitcoin price?"*, *"show my MRs"*.

## ⚠ What this story is, and is not

**It is not new capability.** Nearly all of it is already filed under this epic:

| piece | story | status |
|---|---|---|
| Media peer (headless browser WebRTC) | D-208 | ready — **the keystone** |
| Audio in (attributed speech) | D-209 | ready |
| Audio out (agent speaks, interruptible) | D-210 | blocked on D-208 |
| Screenshare (render a flux surface into the call) | D-211 | blocked on D-208 |
| JaaS / Brave Talk backend | D-206 | in-progress |
| Answer only when addressed | D-207 | **done** |
| Per-speaker identity | C-408 · C-415 | **done** |

**It is the integration.** Five stories that each pass in isolation, assembled live, in front of
colleagues, is precisely where a demo fails. This story owns the seam, the rehearsal and the fallback.

## ⚠ Google Meet research — the intended boundary exists, the implementation does not

**Owner correction, 2026-08-02:** the Thursday venue is Google Meet. Jitsi / Brave Talk is a
separate backend and is not a substitute for that meeting. The intended production shape is also
not a Meet-specific browser driver inside flux: **flux-exchange terminates Google's channel and
flux consumes a vendor-neutral bidirectional stream carrying audio, text, presence and lifecycle
events.** This is the same ownership rule D-231 applies to hosted SIP.

The evidence pass found three distinct things previously collapsed into “we have Google”:

1. [`flux-connectors/providers/google.toml`](https://github.com/codewandler/flux-connectors/blob/main/providers/google.toml)
   declares Gmail, Calendar and Drive. Calendar can return a sensitive `hangoutLink`, but the
   connector does not declare Meet media or in-call chat.
2. flux-exchange can use Google as its **OIDC identity provider** and can host the Google Workspace
   connector. Its own current inventory still says `subscribe`, the agent WebSocket and channel
   termination are unbuilt. A Google credential being present there therefore does not yet imply a
   Google Meet channel.
3. Google's official [Meet REST API](https://developers.google.com/workspace/meet/api/guides/overview)
   manages spaces and exposes participants, events and post-conference artifacts. The separate
   [Meet Media API](https://developers.google.com/workspace/meet/media-api/guides/get-started) is
   Developer Preview, requires the Cloud project, OAuth principal **and every conference participant**
   to be preview-enrolled, and exposes restricted `*.readonly` scopes. Its required WebRTC offer uses
   receive-only audio/video transceivers
   ([concepts](https://developers.google.com/workspace/meet/media-api/guides/concepts)). It can consume
   live audio; it does not provide the outbound voice or bidirectional in-call text this demo needs.

So the desired architecture is sound, but no currently shipped component closes the Google-facing
half:

```text
Google Meet <-> Google-specific terminator in flux-exchange
            <-> authenticated, grant-scoped exchange WebSocket
            <-> flux `connector` channel in `mode = "remote"`
            <-> existing room / voice turn machinery
```

The future frame contract must cover inbound and outbound audio, inbound and outbound text,
participant attribution, join/leave/refusal, ordering, backpressure and cancellation. Google OAuth
credentials stay in exchange. This is a channel API, not C-399 guarded-IO port delegation, and it
must not introduce a second vendor request path.

### Viable non-API option: a browser-backed exchange terminator

Yes: exchange could run a real Chrome participant under a dedicated Google Workspace account and
control the Meet web client over CDP. That closes both directions the read-only Media API leaves
open:

- exchange writes agent audio into a per-session virtual microphone and reads meeting audio from a
  per-session virtual speaker/monitor (PipeWire or PulseAudio); CDP controls the page but is not the
  audio transport;
- exchange drives join/admission, mute and in-call chat through the Meet DOM, then projects chat and
  participant changes onto the same vendor-neutral channel frames;
- flux sees only those frames. The Google account, cookies, browser profile and Meet DOM remain
  exchange-side.

This should use a **dedicated account with an operator-bootstrapped persistent browser profile**, not
automated username/password entry for every call. Google documents that automation-controlled
browsers may be refused at sign-in
([Account Help](https://support.google.com/accounts/answer/7675428)), and Chrome 136+ requires remote
debugging to use a non-default `--user-data-dir`
([Chrome security note](https://developer.chrome.com/blog/remote-debugging-port)). Each tenant/account
therefore needs an isolated profile and browser process (or container), with the CDP pipe/socket kept
local, encrypted profile storage, bounded CPU/memory/output and explicit teardown. Meet itself still
applies admission, organization and microphone-permission rules
([Meet Help](https://support.google.com/meet/answer/9303069),
[audio permissions](https://support.google.com/meet/answer/10620276)).

This is technically feasible, but it is a **browser compatibility integration**, not a supported
Google Meet API contract: DOM changes, reauthentication/MFA, risk challenges, organizer policy and
Google's terms can break it. Before production it needs an acceptable-use decision, participant
disclosure/consent, a canary against live Meet, and a fail-closed fallback when selectors or account
state drift.

**Decision for now:** record and defer this integration. Continue Jitsi hardening on its own merits,
but do not call it Thursday-critical or evidence that the Google Meet path works. With today's public
Google interfaces and today's exchange inventory, the full bidirectional Thursday scenario is not
available; a reduced Slack/text demonstration is the honest fallback.

## Historical Jitsi acceptance — not the Google Meet contract

These criteria remain useful for the separate Jitsi backend. Completing them does **not** satisfy
the Thursday Google Meet scenario described above.

- [ ] The whole scenario runs end to end on Brave Talk / JaaS, **twice, on the demo machine**, from a
      cold start — not from a warm session that happened to work.
- [ ] Slack: `@flux <link>` is acknowledged in-channel *before* joining. ⚠ The acknowledgement is the
      part the audience reads as intelligence; an agent that joins silently and speaks 20 s later looks
      broken. If flux cannot type before it acts, say so rather than faking the delay.
- [ ] Audio out is verified by a **level probe**, not by "unmuted" — D-208's own Acceptance already
      insists on this, and it is the failure that is invisible until you are live.
- [ ] The 2 s black screenshare opening is deliberate and reproducible, not a race that happens to
      look like a pause.
- [ ] ⚠ **A documented fallback for every step**, rehearsed: no audio → flux answers in Slack; no
      screenshare → audio only; sidecar dies → text and presence keep working (D-208 requires this
      already). A demo without a fallback is a coin flip in front of colleagues.
- [ ] The live questions run against **real** ops — a bitcoin price and an MR list — through the normal
      approval envelope, with approvals pre-granted for the demo scope and that fact stated out loud.
      ⚠ Do not demo with the envelope disabled; the envelope *is* the product.
- [ ] The runbook is committed: exact commands, config, what to do when each step fails.
- [ ] ⚠ **Nothing in the demo path requires a credential pasted into a prompt** (see C-432's finding —
      the `Redactor` cannot redact what it was never told about). The screen is shared; whatever is
      typed is published.

## Notes

- Priority **1**: the owner asked for at least parity with the top-ranked story (C-342, priority 2).
  This outranks it because it has a date attached and C-342 does not.
- ⚠ **D-208 is priority 30 today and is the keystone.** Audio in, audio out and screenshare are all
  blocked behind it. Reprioritizing it is the single highest-leverage move for this demo.
- The agent-loop visualization the owner wants for the *second half* of the talk is a separate epic
  ([agent-loop-visibility](../designs/agent-loop-visibility.md)) and is deliberately **not** a
  dependency here. If it lands, it strengthens the demo; if it does not, the demo still runs.
- D-213 (the room safety envelope) is `ready` and relevant: this demo puts flux in a room with
  colleagues who are untrusted co-present principals. Not a blocker, but read it before the day.

## Progress

- Filed 2026-08-01 on the owner's request for a dated demo.
- 2026-08-02 — corrected the venue and ownership after checking all three repositories and Google's
  current official API documentation. The target is Google Meet terminated by flux-exchange, not
  Jitsi in flux. Research only; implementation deliberately deferred.
