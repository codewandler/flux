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

## ⚠ The one thing with no path today: Google Meet

The sidecar is the **Jibri pattern — headless Chrome running `lib-jitsi-meet`** (D-208). That speaks
Jitsi. **Google Meet is not Jitsi and not XMPP**, and the room backends are `mock`, `xmpp` and `jaas`.
There is no Meet backend, and reaching one would mean driving Meet's own web app through CDP — fragile
against every UI change, and against Meet's automation terms.

**Recommendation: run the demo on Brave Talk / JaaS.** From the audience's side the scenario is
identical — a link in Slack, flux appears, waves, speaks. And the hardest risk is already retired: the
**2026-07-30 spike drove a real Brave Talk call from headless Chrome and audio out was confirmed
audible by a human in the call** (D-208's Progress).

Google Meet support, if genuinely wanted, is separate scope and must not be discovered on the day.

## Acceptance

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
