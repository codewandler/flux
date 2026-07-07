---
id: D-79
title: flux-pg owns DSN redaction — a safe-to-print form so no consumer hand-rolls it
pillar: Core
status: done
epic: pg-backend
design: docs/designs/pg-backend.md
note: "every consumer that logs its storage target must currently invent its own redaction; naive string surgery leaks ?password= query params (sqlx honors them!) and mis-splits on '@' in the query — the DSN contract owner should expose the one correct redacted form"
---

# flux-pg owns DSN redaction

## Goal
`flux-pg` parses and owns the DSN contract but exposes no safe-to-print form, so any consumer that
wants to log where it connected must hand-roll redaction over the raw URL. Naive forms get it
wrong in ways that leak secrets: sqlx accepts `password` as a **query parameter** (a valid,
authenticating DSN with no userinfo at all), and an `@` inside the query string defeats
split-at-`@` heuristics. Add one canonical redacted rendering:

```rust
impl PgHandle { pub fn redacted_dsn(&self) -> String }   // or Dsn::redacted() + expose via connect
```

— rebuilt from the *parsed* options (scheme preserved, host:port/db kept, userinfo shown as `…`,
`password`/`sslpassword`-class query params masked, flux-owned params echoed), never from raw
string surgery.

## Acceptance
- [ ] Redacted form built from parsed components; unit tests cover: userinfo credentials,
      `?password=` with no userinfo, both together, `@` inside a query param value,
      `postgresql://` scheme preservation, and flux-owned params surviving visibly.
- [ ] `PgHandle`'s `Debug` stays non-leaking; docs point consumers at the redacted form for
      startup banners/logs.
- [ ] Grep the workspace for any existing raw-DSN printing and switch it.

## Progress
- 2026-07-08 — done. `PgHandle::redacted_dsn() -> &str`, built by a new `redact(&url::Url)` from
  the *parsed* components before the flux-owned params are stripped (so they stay visible):
  scheme preserved, host:port/path kept, userinfo collapsed to `…`, any query param whose key
  contains `password` (case-insensitive → covers `password`, `sslpassword`) masked to `…`,
  everything else echoed. Stored on the handle at connect time (`Dsn.redacted` → field on
  `PgHandle`). `Debug` for `PgHandle` now shows the redacted DSN (and only that) — a `{:?}` can
  never leak, whichever way the credentials arrived. Seven unit tests cover the acceptance matrix
  (userinfo; `?password=` with no userinfo; both; `@` inside a query value; `postgresql://`
  scheme; flux params visible; Debug non-leaking). Bonus fix in scope: `Dsn::parse`'s error for an
  unparseable DSN echoed the raw string (credentials included) — now omits it, with a regression
  test (`parse_error_does_not_echo_the_dsn`). `Dsn` deliberately has no `Debug` (its connect
  options could print the password). Workspace grep found no existing raw-DSN printing to switch;
  the `redacted_dsn` rustdoc is the documented entry point for consumers' startup banners.

## Notes
- Found in a post-ship review: a consumer's hand-rolled `rsplit_once('@')` printed a
  `?password=…` DSN verbatim to stdout at boot.
- Design: [pg-backend.md](../designs/pg-backend.md) §1 (the DSN contract table).
