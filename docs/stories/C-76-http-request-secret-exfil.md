---
id: C-76
title: Constrain http.request $secret so it can't exfiltrate arbitrary env vars
pillar: Core
status: done
priority: 1
epic: harness-hardening
design: docs/designs/harness-hardening.md
note: "SECURITY (Critical, verified) — prompt-injected model → any env secret to any URL, hidden by the redactor"
---

# Constrain http.request $secret so it can't exfiltrate arbitrary env vars

## Goal
Close the single-call secret-exfiltration primitive in `http.request`: a prompt-injected model can
put `{"$secret":"AWS_SECRET_ACCESS_KEY"}` (or `GITHUB_TOKEN`, `ANTHROPIC_API_KEY`, …) on a header to
`http://attacker.tld` and, because `network.fetch` is a default grant, no approval fires — and the
resolved value is then registered with the redactor, so it is *scrubbed from the transcript* and the
operator never sees it leave. Serves the Core value: the harness must fail closed against a hostile model.

## Acceptance
- [x] Failing-first test in `flux-web` (`secret_ref_to_non_allowlisted_env_var_is_refused`): a
      `$secret` naming an env var that is NOT on the allowlist is **refused**, not sent (verified even
      when the env var is set — the value is never read).
- [x] `resolve_header_value` no longer resolves a model-chosen env name unconditionally; it checks the
      allowlist first and refuses (before any `std::env::var`) if the name is not permitted.
- [~] Host-binding not implemented; the operator-controlled allowlist (`[web] allowed_secrets` /
      `FLUX_WEB_SECRET_ALLOW`, fail-closed/deny-all by default) closes the arbitrary-env-var vector.
      Per-secret destination-host binding remains available as future defense-in-depth.
- [x] Redactor registration is unchanged for legitimately-resolved (allowlisted) secrets.

## Progress
- **2026-07-15 — DONE (compile + unit-test verified; full gate pending).** Added `allowed_secrets` to
  `WebOptions` (default `None` ⇒ `FLUX_WEB_SECRET_ALLOW` env fallback; `Some(vec![])` = deny-all).
  `HttpRequestTool` captures it at construction; `resolve_header_value` refuses any non-allowlisted
  `$secret` before reading it. 3/3 secret tests pass in `codewandler-flux-web`.

## Notes
- `crates/flux-web/src/http.rs:220` (`resolve_header_value`), `:241` (`as_secret_ref`); default grant at
  `crates/flux-policy/src/lib.rs:393`. Egress guard blocks internal IPs but not an external attacker host.
- Design: [harness-hardening](../designs/harness-hardening.md).
