---
id: C-04
title: Claude provider verify + force-refresh-on-401
pillar: Core
status: backlog
epic: subscription-providers-and-cost
theme: subscription-providers-cost
design: docs/designs/subscription-providers-and-cost.md
note: refresh today is expiry-time-only; add a 401→refresh→retry path (shared by both subscription providers)
---

# Claude provider verify + force-refresh-on-401

## Goal
Verify the `claude` subscription provider end-to-end (import → refresh → Bearer + `oauth-2025-04-20` beta
+ Claude-Code system prefix) and close the one structural gap: a token is refreshed on **expiry time only**,
never on a 401, so a stale/wrong expiry just fails the request. Add a force-refresh-then-retry path. Applies
to both subscription providers (claude + codex share `RefreshingToken`).

## Acceptance
- [x] **force-refresh on 401.** A 401 from a provider whose credential is OAuth triggers exactly one token
      refresh and one retry of the request; a second 401 surfaces the error (no infinite loop). Failing-first
      test `oauth_401_triggers_single_refresh_and_retry` against a mock backend (401 → refresh → 200) asserts
      one refresh call + one retry. (`crates/flux-provider/src/lib.rs` `NativeProvider` retry path +
      `TokenSource`/`Credential` seam — add a `force_refresh()`/invalidate hook on the refreshing source.)
- [x] **non-401 errors are not retried as auth.** Test `oauth_500_does_not_force_refresh` (5xx uses the
      existing backoff, not a token refresh).
- [x] **claude end-to-end verify.** A hermetic test drives `OAuthAnthropic` through a mock Messages endpoint
      asserting the `authorization: Bearer`, `anthropic-beta: oauth-2025-04-20` headers and the
      `"You are Claude Code…"` system prefix are present. Test `claude_oauth_request_shape`.
- [x] Gate green: `cargo build/test`, `clippy -D warnings`, `fmt`, `cargo test -p flux-codegate`.

## Progress
- **Done.** Force-refresh-on-401 path landed end-to-end.
- **Seam.** Added two default-method extensions: `Credential::token_source() -> Option<Arc<dyn TokenSource>>`
  (default `None`) and `TokenSource::refresh()` (default no-op). Both are non-breaking — every existing
  impl keeps compiling. `OAuthAnthropic` and the codex `OpenAiCred` return their token source;
  API-key/`ollama`/`x-api-key` credentials return `None`.
- **HTTP path.** `NativeProvider::stream` now: on a `401` with an OAuth-backed credential, calls
  `token_source().refresh()` exactly once (`forced_refresh` guard), re-applies the credential — which now
  reads the freshened token — and retries once. A second `401` falls through and surfaces `Error::Api{401}`
  (no infinite loop). 5xx/429 keep the existing backoff and never force a refresh.
- **RefreshingToken.** `refresh()` overrides the default to refresh ignoring the expiry buffer, persisting via
  `save_stored`, and coalesces a concurrent burst (a `FORCE_REFRESH_DEDUP_MS` window keyed on the last
  successful refresh) so a flurry of 401s spends the grant once. Refactored the shared refresh body into
  `refresh_locked` (used by both the lazy expiry path and the forced path).
- **Tests.** `oauth_401_triggers_single_refresh_and_retry`, `oauth_second_401_surfaces_error_no_infinite_loop`,
  `oauth_500_does_not_force_refresh` (flux-provider); `claude_oauth_request_shape` (flux-providers);
  `force_refresh_ignores_expiry_buffer_and_coalesces`, `force_refresh_without_refresh_token_errors`
  (flux-credentials). All hermetic (TcpListener stubs + mock refresher). Full gate green.

## Notes
- Epic + design: [subscription-providers-and-cost.md](../designs/subscription-providers-and-cost.md).
- Touch points: `crates/flux-provider/src/lib.rs` (`NativeProvider::stream` retry/backoff —
  `is_retryable_status`/`backoff_delay`; thread a 401→force-refresh branch), the `TokenSource`/`Credential`
  traits, `crates/flux-credentials/src/lib.rs` (`RefreshingToken` — expose an explicit force-refresh that
  ignores the expiry buffer), `crates/flux-providers/src/anthropic.rs` (`OAuthAnthropic`).
- Reuse: the existing per-attempt `Credential::apply` re-application (already re-applies auth each retry —
  the missing piece is that the refresh only fires on expiry, so a 401 with a "valid" expiry re-sends the
  same dead token).
- Keep refresh serialized behind the existing async mutex; a concurrent burst of 401s must refresh once.
