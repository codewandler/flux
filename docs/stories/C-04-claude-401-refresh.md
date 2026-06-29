---
id: C-04
title: Claude provider verify + force-refresh-on-401
pillar: Core
status: backlog
priority:
design: docs/designs/subscription-providers-and-cost.md
theme: subscription-providers-cost
---

# Claude provider verify + force-refresh-on-401

## Goal
Verify the `claude` subscription provider end-to-end (import → refresh → Bearer + `oauth-2025-04-20` beta
+ Claude-Code system prefix) and close the one structural gap: a token is refreshed on **expiry time only**,
never on a 401, so a stale/wrong expiry just fails the request. Add a force-refresh-then-retry path. Applies
to both subscription providers (claude + codex share `RefreshingToken`).

## Acceptance
- [ ] **force-refresh on 401.** A 401 from a provider whose credential is OAuth triggers exactly one token
      refresh and one retry of the request; a second 401 surfaces the error (no infinite loop). Failing-first
      test `oauth_401_triggers_single_refresh_and_retry` against a mock backend (401 → refresh → 200) asserts
      one refresh call + one retry. (`crates/flux-provider/src/lib.rs` `NativeProvider` retry path +
      `TokenSource`/`Credential` seam — add a `force_refresh()`/invalidate hook on the refreshing source.)
- [ ] **non-401 errors are not retried as auth.** Test `oauth_500_does_not_force_refresh` (5xx uses the
      existing backoff, not a token refresh).
- [ ] **claude end-to-end verify.** A hermetic test drives `OAuthAnthropic` through a mock Messages endpoint
      asserting the `authorization: Bearer`, `anthropic-beta: oauth-2025-04-20` headers and the
      `"You are Claude Code…"` system prefix are present. Test `claude_oauth_request_shape`.
- [ ] Gate green: `cargo build/test`, `clippy -D warnings`, `fmt`, `cargo test -p flux-codegate`.

## Progress
- (not started)

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
