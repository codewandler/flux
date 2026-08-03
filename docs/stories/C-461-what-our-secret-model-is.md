---
id: C-461
title: "Say what flux's secret model is — including that the connector path already prevents while the local path only contains"
pillar: Core
status: done
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

**flux undersells prevention — and a survey found it does this in THREE places, not one:**

1. **The connector boundary** ([C-312](C-312-connector-credential-boundary.md)) — *"flux holds exactly
   ONE secret on this path… a response carrying credential-shaped material is refused, not merely
   redacted."* The vendor credential never enters the process.
2. **[`conn.authenticate` (D-31)](D-31-host-terminated-rawsocket-auth.md)** — for Postgres, **the host
   speaks the SCRAM handshake itself so the plugin never receives the password at all**
   (`crates/flux-plugin/src/host.rs:986+`). That is prevention by protocol, and it is a better story
   than any redaction claim.
3. **`ResolvedEndpoint` has no `Serialize` impl at all** (`crates/flux-secret/src/endpoint.rs:145`) — a
   **compile-time** guarantee that a materialized credential cannot be serialized toward the model,
   while `EndpointRef` carries only a *location*.

⚠ All three are the property Vaults sells as its headline — *"the agent never sees the secret value"* —
and flux states them only in story files and module headers.

**flux oversells the local path by omission.** `secret "NAME"` resolves to plaintext in process
(`crates/flux-app/src/secrets.rs:35-41`, substituted in place at `:52`) and relies on the `Redactor`
afterwards. That is a real defence and a **containment** one, with a failure mode worth knowing: three
shipped bugs, one of which ([C-339](C-339-redaction-falls-back-to-the-unredacted-value.md)) failed
**open**.

⚠ **And one gap is structural rather than a bug, already documented in the tree and nowhere a user
looks**: `crates/flux-cli/src/export_cmd.rs:21-28` states that conversation `Message` text and
`TurnSummary.user_input`/`answer` are written **with the raw prompt and no redactor in the path** — the
only control for those fields is the shape-based pass at export time. That is exactly C-432's mechanism,
and it is the single most important sentence for a user to read.

## Acceptance

- [x] A page stating the two models and which secrets fall under each. ⚠ **A reader must be able to
      determine, for their own secret, which one they have** — that is the entire point.
- [x] ⚠ **The `Redactor`'s limit is stated plainly: it redacts values it has been told about.** A
      credential pasted into a prompt, or read from a file flux did not resolve, is not redacted. This is
      [C-432](C-432-browser-credentials-never-come-from-the-prompt.md)'s finding and it is the single
      most useful sentence on the page.
- [x] The connector-path guarantee is stated where a user evaluating flux will meet it, not only in a
      story.
- [x] ⚠ **No overclaiming.** *"Secrets are redacted from model-visible output"* is true and incomplete;
      unqualified, it invites the belief that pasting a key into a prompt is safe. The repo's register is
      `vision.md`'s — it calls a pillar *"currently aspirational, and this document says so honestly."*
- [x] Where flux is genuinely behind, say so and link the story — destination scoping reaches only
      `http.request` ([C-459](C-459-scope-a-secret.md); local program/channel secrets remain unscoped;
      flux also scopes which secret may be *named* via `allowed_secrets` and plugin `grants.secrets`),
      no local-program-secret rotation without restart, and audit of use covering exactly one hop
      ([C-460](C-460-rotation-revocation-audit.md)).
- [x] ⚠ The **documented, structural** exception is stated: raw prompt text and turn summaries reach the
      durable log with no redactor in the path. A page that lists the guarantees without this one is the
      overclaim it exists to prevent.
- [ ] Full gate green including website checks. (Website checks are green; the wave integration parent
      owns the single full repository gate.)

## Notes

- Feeds [C-442](C-442-peer-docs-gap-audit.md): "how are secrets handled" is exactly the obvious-to-
  everyone-else topic that audit is looking for, and this is the second instance after context
  management ([C-441](C-441-context-management-doc.md)).
- ⚠ Not a competitor comparison. [C-429](C-429-the-recipes-surface-and-positioning.md) settled that
  public positioning argues from architecture and names no competitor. Vaults is the *reason* this page
  is being written; it should not appear on it.

## Progress
- Filed 2026-08-02 from the Vaults comparison.
- 2026-08-03 — rewrote the public credentials guide around a user-classifiable prevention versus
  containment table. It distinguishes process prevention for platform-sourced connector vendor
  credentials, plugin prevention for PostgreSQL `conn.authenticate`, and the non-serializable
  `ResolvedEndpoint` boundary from local plaintext program/provider/plugin paths. The guide states
  the `Redactor`'s registration limit, heuristic gaps, prompt/answer durable-log exception,
  `http.request`-only destination/principal scope, restart requirement for program-secret rotation,
  and the single cross-plugin audit hop. Corrected the security overview/plain-language page and
  added maintainer/customer changelog entries. Verification: `git diff --check`;
  `cargo test -p codewandler-flux-secret` (23 passed); the plugin credential-boundary unit filter (9
  passed); and `npm run build` in `website/` (production build green after `npm ci`). `npm ci` reported
  the lockfile's existing 27 audit findings (1 low, 20 moderate, 6 high); dependencies were not changed.
  The wave parent owns the single full repository gate.
