---
id: D-93
title: Plugin security surface — secret-field redaction + minimal gitlab.test identity
pillar: Core
status: done
priority: 1
epic: gitlab-plugin-hardening
design: docs/designs/gitlab-plugin-hardening.md
note: "dry-run echoes secret-like CI/pipeline variable values with no redaction metadata (leak to scrollback/logs/transcripts); gitlab.test returns the full ~50-key user profile (email, 2FA, sign-in times) for an auth smoke check (GL-016/031)"
---

# Plugin security surface — secret-field redaction + minimal gitlab.test identity

## Goal
Stop the plugin surface from exposing more secret/identity material than needed: let a field be
marked secret-like so it is redacted wherever inputs are echoed (dry-run, logs, audit), and trim the
`gitlab.test` auth smoke check to a minimal identity.

## Why (evidence)
A beta pass found `--dry-run` echoes `ci.variable.create`/`pipeline.create` variable `value` fields
verbatim inside `input` — so a safe-looking preview of a secret write leaks the value into terminal
scrollback, logs, or saved transcripts — and the plugin schema has no redaction metadata to prevent
it. Separately, `gitlab.test` (an auth smoke test) returns the full ~50-key GitLab user profile
including email, public/commit email, sign-in timestamps, and two-factor status.

## Acceptance
- [ ] `host-kit` gains schema-level redaction metadata for secret-like fields; the dry-run/echo path
      masks them (e.g. `xxxxx`) instead of printing the value (GL-031).
- [ ] `gitlab`'s CI/pipeline variable `value` fields carry that redaction metadata; a failing-first
      test asserts a dry-run of a variable write does not echo the value.
- [ ] `gitlab.test` returns a minimal identity (status + a small, documented identity subset) rather
      than the full profile object; a test pins the returned key set (GL-016).
- [ ] `cargo build/test/clippy -D warnings/fmt` green for `host-kit` + `gitlab` and the plugins
      workspace.

## Progress
- Not started.

## Notes
- Redaction metadata is cross-plugin (host-kit); other plugins with secret-carrying inputs
  (e.g. CI variables, tokens) inherit it. Aligns with the repo's redaction invariant work
  (cf. D-65).
