---
id: C-464
title: "`[web] allowed_secrets` is never wired — the error message tells operators to edit a key that does nothing"
pillar: Core
status: ready
priority: 4
design: docs/designs/secrets-the-agent-never-sees.md
epic: secrets-the-agent-never-sees
areas: [flux-cli, flux-web, docs]
note: "⚠ found by C-459. `crates/flux-cli/src/execution.rs:1741` passes `allowed_secrets: None` unconditionally, so the ONLY working control for this security boundary is `FLUX_WEB_SECRET_ALLOW` — which sits on NON_PUBLIC_ENV and is therefore undocumented. A documented dead key plus an undocumented live one is the worst pairing"
---

# The control that is documented does nothing; the one that works is a secret

## Goal

Make the `$secret` allowlist configurable through the documented path, and document whichever paths are
real.

## The finding

C-76 built a genuine security control: `http.request` refuses to resolve a `$secret` whose name is not
on an allowlist, **before the value is read** — so a prompt-injected model cannot name
`AWS_SECRET_ACCESS_KEY` and exfiltrate it in one call. The control works. Reaching it does not.

- `crates/flux-cli/src/execution.rs:1741` passes **`allowed_secrets: None`** unconditionally. Nothing
  reads a `[web] allowed_secrets` config key into it.
- The refusal message names that key anyway — it tells an operator to *"Add it to `[web]
  allowed_secrets`"*, which will not change the outcome.
- The only live source is the environment variable `FLUX_WEB_SECRET_ALLOW`, and that sits on
  `NON_PUBLIC_ENV` in `crates/flux-cli/tests/website_contract.rs` — i.e. it is deliberately kept out of
  the public docs.

⚠ **A documented key that does nothing, plus an undocumented variable that does everything, is worse
than either alone**: an operator who follows the error message believes they have configured a security
boundary and has not.

## Acceptance

- [ ] **Failing-first**: a test asserting a `[web] allowed_secrets` value in config reaches
      `http.request`'s allowlist — failing at the merge base, where `execution.rs:1741` hard-codes
      `None`.
- [ ] Config wiring exists, or ⚠ **the error message stops naming a key that does not work.** Either
      resolution is acceptable; leaving the message pointing at dead config is not.
- [ ] Whatever ends up being the real control is **documented**. If `FLUX_WEB_SECRET_ALLOW` stays a
      live control it comes off `NON_PUBLIC_ENV`; if it becomes an internal escape hatch, config is the
      documented path.
- [ ] ⚠ **The default is unchanged.** This story is about *reachability*, not about tightening or
      loosening what is allowed — do not quietly change the deny/allow default while wiring it.
- [ ] Full gate green.

## Notes

- Interacts with [C-459](C-459-scope-a-secret.md), which extends the *entry grammar* of the same
  allowlist (`NAME;to=host`). ⚠ Land order matters: wiring config after the grammar changed means the
  config parser must accept the extended form, not just a bare name.
- ⚠ **Checked and NOT a finding**, recorded so nobody re-files it: C-459's report also flagged
  `crates/flux-channels/src/adapters/mod.rs` as *"a second `as_secret_ref` resolution site with no
  allowlist at all."* It is not a resolution site — `first_unresolved_secret` is a **detector** that
  turns an unresolved marker into a clear error instead of an opaque serde failure. It reads markers; it
  never resolves one.

## Progress

- Filed 2026-08-02 from C-459's adjacent findings, after verifying both halves at `execution.rs:1741`
  and confirming the third claimed finding was not one.
