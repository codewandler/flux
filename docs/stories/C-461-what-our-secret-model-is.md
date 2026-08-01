---
id: C-461
title: "Say what flux's secret model is — including that the connector path already prevents while the local path only contains"
pillar: Core
status: ready
priority: 6
design: docs/designs/secrets-the-agent-never-sees.md
epic: secrets-the-agent-never-sees
areas: [docs, website]
note: "⚠ flux tells a weaker story than it has earned. C-312 proved that on the connector path the vendor credential NEVER ENTERS the process — that is the strong model, asserted with a test — and nothing user-facing says so. Meanwhile the local path's honest limit is undocumented"
---

# Two models, one of them unadvertised and one of them unqualified

## Goal

State flux's secret model where a user will read it: what is prevented, what is merely contained, and
what the `Redactor` can and cannot do.

## Why both halves are wrong today

**flux undersells the connector path.** [C-312](C-312-connector-credential-boundary.md) asserts, with a
test, that *"flux holds exactly ONE secret on this path — the deployment session bearer. A response
carrying credential-shaped material is refused, not merely redacted."* The vendor credential never
enters the process. That is the same property Anthropic's Vaults sells as its headline — *"the agent
never sees the secret value"* — and flux says it only inside a story file.

**flux oversells the local path by omission.** `secret "NAME"` resolves to plaintext in process
(`crates/flux-app/src/secrets.rs:35-41`) and relies on the `Redactor` afterwards. That is a real
defence and a **containment** one, with a failure mode worth knowing: three shipped bugs, one of which
([C-339](C-339-redaction-falls-back-to-the-unredacted-value.md)) failed **open**.

## Acceptance

- [ ] A page stating the two models and which secrets fall under each. ⚠ **A reader must be able to
      determine, for their own secret, which one they have** — that is the entire point.
- [ ] ⚠ **The `Redactor`'s limit is stated plainly: it redacts values it has been told about.** A
      credential pasted into a prompt, or read from a file flux did not resolve, is not redacted. This is
      C-432's finding and it is the single most useful sentence on the page.
- [ ] The connector-path guarantee is stated where a user evaluating flux will meet it, not only in a
      story.
- [ ] ⚠ **No overclaiming.** *"Secrets are redacted from model-visible output"* is true and incomplete;
      unqualified, it invites the belief that pasting a key into a prompt is safe. The repo's register is
      `vision.md`'s — it calls a pillar *"currently aspirational, and this document says so honestly."*
- [ ] Where flux is genuinely behind, say so and link the story — no destination scoping (C-459), no
      rotation-without-restart or audit of use (C-460).
- [ ] Full gate green including website checks.

## Notes

- Feeds [C-442](C-442-peer-docs-gap-audit.md): "how are secrets handled" is exactly the obvious-to-
  everyone-else topic that audit is looking for, and this is the second instance after context
  management ([C-441](C-441-context-management-doc.md)).
- ⚠ Not a competitor comparison. [C-429](C-429-the-recipes-surface-and-positioning.md) settled that
  public positioning argues from architecture and names no competitor. Vaults is the *reason* this page
  is being written; it should not appear on it.

## Progress
- Filed 2026-08-02 from the Vaults comparison.
