---
id: D-229
title: "What redaction cannot reach — spoken secrets and DTMF are not text, and the `Redactor` only knows text"
pillar: Agent
status: ready
priority: 8
design: docs/designs/sip-channel.md
epic: sip-channel
areas: [flux-channels, flux-secret, docs]
note: "⚠ the disclosure story. The Redactor operates on TEXT; a spoken secret in recorded audio is redactable by nothing flux has. And DTMF is how people enter PINs and card numbers — sipx supports DTMF, so flux WILL receive them"
---

# A secret said out loud

## Goal

Decide — and enforce — what flux records from a call, given that its redaction machinery cannot reach
audio at all.

## ⚠ The gap, stated plainly

flux's safety story includes *"secrets redacted from model-visible output and never off the machine."*
The `Redactor` does that **on text**. A phone call carries two things it cannot touch:

1. **Spoken secrets.** Someone reads a password, a token or an account number aloud. In recorded audio
   that value is redactable by nothing flux currently has.
2. ⚠ **DTMF.** Keypad tones are *how people enter PINs and card numbers*, and sipx supports DTMF — so
   flux will receive them. A DTMF sequence arriving as digits is a credential in the plainest possible
   form, and it will land in whatever the channel records.

A transcript makes it worse, not better: transcription turns spoken secrets into text that then flows
everywhere text flows.

## Acceptance

- [ ] **Failing-first**: a test asserting a DTMF sequence does not land unprotected in the durable
      record — failing at the merge base.
- [ ] ⚠ **DTMF is treated as potentially-secret by default.** Not "redacted if it looks like a card
      number" — a heuristic here fails open, and
      [C-339](C-339-redaction-falls-back-to-the-unredacted-value.md) is this repo's evidence that
      fail-open redaction ships.
- [ ] What audio, if any, is retained is an explicit decision with a default that **refuses rather than
      retains** what cannot be redacted.
- [ ] If transcription happens, the transcript is subject to the `Redactor` on the same terms as any
      other model-visible text, and the **failure path is tested** — not just the happy one.
- [ ] ⚠ **The limitation is documented where an operator enabling call recording will read it.** An
      operator who assumes flux's redaction covers a call recording has assumed something false, and
      the consequence is a stored credential.
- [ ] Consent and notice obligations are named as an operator responsibility, with jurisdiction-specific
      advice explicitly out of scope. ⚠ Recording a call has legal requirements that vary; flux should
      not imply it has handled them.
- [ ] Full gate green.

## Notes

- Settleable ahead of [D-225](D-225-the-sip-sidecar-seam.md), and better decided before the transport
  exists — once audio flows, "we will sort recording out later" becomes a stored liability.
- ⚠ Sibling gap, worth checking at the same time: rooms (D-209/D-210) carry the same problem the moment
  they record. If the answer generalizes, put it in the voice machinery
  ([D-228](D-228-one-voice-turn-machinery.md)), not in the SIP adapter.
- [C-432](C-432-browser-credentials-never-come-from-the-prompt.md) is the same class from the other
  direction: the `Redactor` cannot redact what it was never told about.

## Progress

- Filed 2026-08-01 with the sip-channel epic.
