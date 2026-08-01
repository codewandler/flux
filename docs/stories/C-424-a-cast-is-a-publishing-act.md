---
id: C-424
title: "A cast leaves the machine — redact the rendered frames, not just the recorded payloads"
pillar: Core
status: ready
priority: 12
design: docs/designs/session-screencast.md
epic: session-screencast
areas: [flux-tui, flux-secret]
note: "the epic's safety story, deliberately NOT folded into C-423. `flux replay`'s output stays local; a cast's whole purpose is to be published, and C-339 already found redaction failing OPEN in this codebase. Rendering is a second chance to leak: a secret can be absent from a payload and present in a wrapped, ANSI-styled frame reassembled from it"
---

# Rendering is a second chance to leak

## Goal

A rendered cast cannot carry a secret, proven by a test that attacks the **rendered frames** rather
than the recorded payloads — and a cast that cannot be redaction-checked is **refused**, not written.

## Why this is not part of C-423

Every other Time Machine verb produces local output. `flux replay` re-executes into your own terminal;
`flux diff` prints to your own stdout. **A screencast's entire purpose is to leave the machine** — into
a blog post, a docs page, a conference talk. That changes the blast radius of a redaction failure from
"a log on your disk" to "unrecoverable".

And redaction has failed **open** in this codebase before:
[C-339](C-339-redaction-falls-back-to-the-unredacted-value.md) found `redact_and_hash_request` doing
`unwrap_or(canonical)` — when text-level redaction corrupted the JSON badly enough that it stopped
parsing, the fallback handed back the **original, with the credential intact**. Silent, and fails open.

⚠ **The specific hazard here is that rendering reassembles text.** Capture-time redaction operates on a
payload. A renderer takes that payload, wraps it to a viewport width, injects ANSI styling, and splits
it across frames. A secret can be absent from the payload representation the redactor examined and
present in the frame — and a test that checks payloads will pass while the published GIF leaks.

## Acceptance

- [ ] **Failing-first**: a test planting a credential-shaped value in a fixture session, rendering it,
      and asserting the value is absent from the **rendered frames**, failing at the merge base.
- [ ] ⚠ The assertion is over rendered frame text — ANSI-styled, wrapped, split as the renderer
      actually emits it — **not** over event payloads. A payload-level assertion does not close this
      and must not be substituted.
- [ ] The `Redactor` runs over rendered output at render time, as a second independent pass rather than
      trusting capture-time redaction alone. Two independent passes is the point; one pass moved is not
      a fix.
- [ ] ⚠ **Fails closed.** If the redactor is unavailable, errors, or cannot be constructed, `flux cast`
      **refuses to write** rather than writing an unredacted cast. Pin that with a test that makes the
      redactor fail — C-339's lesson is that the fallback path is where the leak lives, so the fallback
      path is what needs the test.
- [ ] Adversarial cases beyond a plain literal, each pinned or explicitly recorded as out of scope: a
      secret split across a wrap boundary; a secret inside a tool argument echoed into a card header; a
      secret in an error message rendered as a notice; a secret in a plan rendered in the plan pane.
- [ ] The failure names redaction as the reason, so a refused cast is not mistaken for a broken
      renderer.
- [ ] Full gate green.

## Notes

- Pairs with [C-423](C-423-flux-cast.md) but is **not** blocked by it: the redaction pass and its
  adversarial tests can be built against C-422's projection output. If C-423 lands first it must ship
  with an explicit "not redaction-gated" banner (stated in its Acceptance) rather than quietly
  producing publishable files.
- `AGENTS.md` names redaction a safety invariant; this story tightens it and must never relax it to make
  a demo render.
- The wrap-boundary case is the one most likely to be missed and the one most likely to be real — a
  40-character token in an 80-column viewport is not a hypothetical.
- Worth checking whether `OpRecorded`'s 1 MiB cap can truncate *mid-secret*, leaving a partial value
  that the redactor's pattern no longer matches but a human still recognises.

## Progress

- Filed 2026-08-01 with the session-screencast epic.
