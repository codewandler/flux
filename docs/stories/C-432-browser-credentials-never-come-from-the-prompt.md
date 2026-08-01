---
id: C-432
title: "A password in the prompt is redacted by nothing — browser credentials must come from the secret store"
pillar: Core
status: ready
priority: 11
design: docs/designs/explore-then-freeze.md
epic: explore-then-freeze
areas: [flux-secret, flux-web, docs]
note: "⚠ the epic's safety story. `Redactor` redacts values it has been TOLD about; a password typed into a prompt was never registered, so it reaches the model context and the durable event log unredacted. The motivating phrasing for this whole epic — 'log in as X with password Y' — is the exact leak"
---

# The redactor cannot redact what it was never told

## Goal

Make the safe path the only documented one: a browser login resolves its credential from the secret
store, the value is registered with the `Redactor` before the run, and a frozen script **references**
it rather than embedding it.

## Why this is filed alongside the feature, not after it

The phrasing that motivates this epic is *"go to site X, log in as X with password Y."* Followed
literally, that password:

1. enters the model context,
2. lands in the durable event log,
3. is carried into `plan_source`, and
4. can be emitted verbatim into a distilled script that someone then commits.

flux redacts secrets from model-visible output and never lets them off the machine — but the
`Redactor` redacts **values it has been told about** (`Redactor::add_secret`,
`crates/flux-secret/src/lib.rs:287`). A password typed into a prompt was never registered. **Nothing
redacts it.**

⚠ This is a documentation hazard at least as much as a code one. A compelling recipe that teaches
users to paste production passwords into prompts would do more harm than this epic does good — and the
recipe is the whole point of the epic.

## Acceptance

- [ ] **Failing-first**: a test that drives a browser login through the supported path and asserts the
      credential value appears in **neither** the event log **nor** the distilled script — failing at
      the merge base.
- [ ] A browser login resolves its credential from the secret store, and the value is registered with
      the `Redactor` **before** it can reach a log or a model.
- [ ] A distilled script emits a **reference**, never the value. ⚠ Pin that with a test over the
      emitted text: this is where a leak is committed to a repository and becomes permanent.
- [ ] ⚠ **The prompt path is addressed, not just avoided.** Users will paste a password anyway. Decide
      what flux does — detect and warn, refuse, or redact-on-sight — and implement that decision.
      "The docs say not to" is not a control. Whatever is chosen, the *reason* is recorded, because a
      later reader will otherwise assume it was never considered.
- [ ] Every doc, recipe and example in this epic uses only the supported path. No sample string that
      looks like a working password-in-prompt, even in prose — samples get copied.
- [ ] `names_a_secret` (`crates/flux-secret/src/lib.rs:437`) is checked for whether it already covers
      any of this; extend rather than duplicate.
- [ ] Full gate green.

## Notes

- ⚠ Adjacent and worth checking while here: **`plan_source` is redacted at record time**
  (`crates/flux-cli/src/export_cmd.rs:16-18`), which protects registered secrets only. The distiller
  inherits exactly that guarantee and no more — do not let its docs imply otherwise.
- [C-339](C-339-redaction-falls-back-to-the-unredacted-value.md) is the cautionary precedent: redaction
  in this codebase has failed **open** before, returning the unredacted value when redacted text
  stopped parsing. Assume the same class is possible here and test the failure path, not only the happy
  one.
- The browser stack already routes every subrequest through the `web` egress guard via CDP `Fetch`
  interception, so a credential cannot be exfiltrated to an unapproved host by the page itself. That
  covers a different threat from this one — worth stating in the docs so the two are not confused.
- Priority 11 rather than 12: this gates what the epic's documentation is allowed to say, so it should
  not land last.

## Progress

- Filed 2026-08-01 with the explore-then-freeze epic.
