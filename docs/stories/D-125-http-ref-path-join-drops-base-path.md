---
id: D-125
title: "Ref-based http path join drops the endpoint base path (slack 404s)"
pillar: Core
status: done
note: "compose_url uses RFC-3986 Url::join → leading-slash op path REPLACES the base path; MockHost concats — tests green, live slack.com 404 on every op"
---

# Ref-based http path join drops the endpoint base path (slack 404s)

## Goal
A plugin's ref-based `http.do` (`endpoint_ref` + `path`) must join the op path **onto** the resolved
endpoint base — never silently drop the base's path segment. Today `compose_url`
(`crates/flux-plugin/src/lib.rs`) uses RFC-3986 `url::Url::join`, so a leading-slash path *replaces*
the base path: `slack.endpoint` default `https://slack.com/api` + `/auth.test` →
`https://slack.com/auth.test` → 404 HTML for **every** slack op. The host-kit `MockHost` joins by
slash-normalized concatenation, so the plugin's tests pass while the live call fails — the mock and
the real host must share one join semantic.

## Acceptance
- [ ] Failing-first unit tests on `compose_url`: path-bearing base + leading-slash path appends
      (`https://slack.com/api` + `/auth.test` → `https://slack.com/api/auth.test`); host-only base +
      absolute path unchanged (`https://gitlab.com` + `/api/v4/x` → `https://gitlab.com/api/v4/x`);
      trailing-slash base + relative path; `None`/empty path returns the base unchanged.
- [ ] `compose_url` joins by slash-normalized concatenation (the `MockHost::join_url` / OAuth
      `token_path` semantic) — one join rule everywhere; a full-URL `path` no longer silently
      replaces the pinned endpoint base (it fails URL parsing in the egress guard instead).
- [ ] Live proof: `flux plugin call slack slack.test` returns `ok` against the real Slack API with
      the already-shipped pack plugin binary (no plugin rebuild needed — the fix is host-side).

## Progress
- 2026-07-10 filed from a live repro: `flux plugin call slack slack.test` → `POST slack.endpoint
  /auth.test → 404 <!DOCTYPE html>…` (slack.com HTML error page); root-caused to `compose_url`'s
  `Url::join` vs `MockHost::join_url` divergence.
- 2026-07-10 **DONE.** Failing-first test `compose_url_appends_path_onto_path_bearing_base`
  (asserted the live bug: left `https://slack.com/auth.test`), then `compose_url` switched to
  slash-normalized concatenation (base parse-check kept so a broken endpoint binding still errors
  as a base error). Full flux-plugin suite green. Live proof with the UNCHANGED v0.1.0 pack slack
  binary: `flux plugin call slack slack.test` → `status: ok` for both tokens, and
  `slack.channel.list` (a different API path) returns real channels. CHANGELOG + WHATS-NEW updated
  (fix → patch bump next cut). Not committed (awaiting instruction).

## Notes
- Repro: any slack op via `flux plugin call slack …` with the v0.1.0 pack binary.
- `compose_url`: `crates/flux-plugin/src/lib.rs:2170`; mock join: `plugins/host-kit/src/lib.rs`
  (`join_url`); precedent: the OAuth `token_path` join in `resolve_purpose`
  (`crates/flux-plugin/src/lib.rs:937-941`).
- gitlab/jira survive today only because their endpoint bases are host-only, so RFC join and concat
  agree; concat is also strictly safer (a full-URL `path` can no longer escape the pinned base).
