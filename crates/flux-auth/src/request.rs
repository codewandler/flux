//! `flux_auth::request` — the per-request bearer→principal seam.
//!
//! [`RequestAuthenticator`] is the per-request sibling of the ambient [`crate::IdentityProvider`]:
//! it authenticates **one bearer token per call** and yields an [`AuthContext`] carrying the
//! *already-projected* `(Caller, Trust)`. That keeps exactly one claims→identity projection
//! point (the authenticator), so no surface can invent its own mapping — and the safety envelope
//! stays the sole authorization source of truth: an `AuthContext` makes no decisions, it only
//! feeds the same inputs the CLI feeds today. Design: `docs/designs/request-auth-seam.md`.

use async_trait::async_trait;
use flux_policy::{Caller, Trust};

/// Maximum accepted bearer token length in bytes. Enforced *before* any hashing or network work
/// so an attacker-supplied header bounds our CPU as well as theirs.
const MAX_TOKEN_BYTES: usize = 8192;

/// Request-scoped identity: everything a multi-tenant surface needs from one bearer token.
///
/// `account` is a **tenancy/storage key only** — it flows to the event substrate to tag runs and
/// scope session reads; policy never consults it. Account-conditional *grants* work via the
/// mirror group `account:<id>` in `caller.groups`, and that prefix is reserved: the
/// authenticator is its only writer.
#[derive(Debug, Clone)]
pub struct AuthContext {
    /// Tenancy key (→ the event store's account tag); **not** a policy input.
    pub account: Option<String>,
    /// Principal + roles-as-groups — the identity the safety envelope evaluates.
    pub caller: Caller,
    /// Trust level (≤ `Verified` for any network-presented token) + token scopes.
    pub trust: Trust,
}

/// Why per-request authentication failed.
///
/// **Error hygiene:** [`AuthError::Unavailable`]'s payload is **log-only** diagnostic detail —
/// backend error text can carry internal endpoint URLs, and interpolating anything into a
/// response header is CRLF injection. Wire responses (body *and* headers) must be constant
/// strings; 401 responses carry the byte-constant [`WWW_AUTHENTICATE`] challenge. Surfaces map
/// `Unavailable` to 502/503 as fits their topology — fail closed either way; there is no
/// anonymous fallback identity.
#[derive(Debug, Clone, PartialEq)]
pub enum AuthError {
    /// The token is invalid / expired / inactive / of the wrong type → HTTP 401.
    Unauthorized,
    /// The authenticator's backend failed (timeout, 5xx, malformed response). Never a
    /// pass-through: the payload is for logs only, wire bodies stay constant.
    Unavailable(String),
}

/// The byte-constant `WWW-Authenticate` value every 401 carries (RFC 6750). Identical across all
/// rejection causes so a response can never leak *why* a token was refused.
pub const WWW_AUTHENTICATE: &str = "Bearer error=\"invalid_token\"";

/// Authenticates one bearer token into an [`AuthContext`].
///
/// Takes the **bare token, not the header** — tokens legitimately arrive over non-header
/// transports (e.g. a WebSocket subprotocol); HTTP surfaces extract uniformly via
/// [`bearer_from_header`]. Object-safe (`Arc<dyn RequestAuthenticator>`), and a sibling of the
/// ambient [`crate::IdentityProvider`], not its replacement: both terminate in the same
/// `(Caller, Trust)` the envelope evaluates.
#[async_trait]
pub trait RequestAuthenticator: Send + Sync {
    /// Authenticate `bearer` into a request-scoped identity. Fail closed: invalid tokens are
    /// [`AuthError::Unauthorized`], backend trouble is [`AuthError::Unavailable`].
    async fn authenticate(&self, bearer: &str) -> Result<AuthContext, AuthError>;

    /// Like [`Self::authenticate`], additionally surfacing the token's expiry (seconds since the
    /// Unix epoch) when the backend knows it, so a caching decorator can bound its TTL to the
    /// token lifetime. The default delegates to `authenticate` and reports no expiry —
    /// implementors whose backend exposes `exp` override this; everyone else is
    /// default-compatible and the trait stays object-safe.
    async fn authenticate_with_expiry(
        &self,
        bearer: &str,
    ) -> Result<(AuthContext, Option<u64>), AuthError> {
        self.authenticate(bearer).await.map(|ctx| (ctx, None))
    }
}

/// RFC 7235/6750 bearer extraction from an `Authorization` header value.
///
/// Accepts `Bearer <token>` with a case-insensitive scheme and **exactly one** space separator;
/// the token must be non-empty, at most 8 KiB, and pure RFC 6750 `b64token` (ALPHA / DIGIT /
/// `-` `.` `_` `~` `+` `/`, plus trailing `=` padding). Everything else — including whitespace,
/// control bytes, or unicode inside the token — is [`AuthError::Unauthorized`], decided before
/// any hashing or network work.
///
/// **Surface contract:** a request presenting more than one `Authorization` header must be
/// rejected by the surface *before* extraction — this helper sees a single value and cannot
/// detect duplicates (smuggling-style proxy divergence otherwise).
pub fn bearer_from_header(header: Option<&str>) -> Result<&str, AuthError> {
    let header = header.ok_or(AuthError::Unauthorized)?;
    let (scheme, token) = header.split_once(' ').ok_or(AuthError::Unauthorized)?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return Err(AuthError::Unauthorized);
    }
    // Length first: bounds the charset scan (and everything after it) on attacker input.
    if token.is_empty() || token.len() > MAX_TOKEN_BYTES {
        return Err(AuthError::Unauthorized);
    }
    if !is_b64token(token) {
        return Err(AuthError::Unauthorized);
    }
    Ok(token)
}

/// RFC 6750 `b64token`: `1*( ALPHA / DIGIT / "-" / "." / "_" / "~" / "+" / "/" ) *"="` — at
/// least one core character, `=` only as trailing padding. A second space separator surfaces
/// here too: space is not a `b64token` character.
fn is_b64token(token: &str) -> bool {
    let core = token.trim_end_matches('=');
    if core.is_empty() {
        return false;
    }
    core.bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'+' | b'/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-time proof the trait stays object-safe (`Arc<dyn RequestAuthenticator>` is the
    /// intended consumption shape).
    #[test]
    fn trait_is_object_safe() {
        fn _takes_dyn(_a: &dyn RequestAuthenticator) {}
    }

    #[test]
    fn extracts_a_valid_bearer_token() {
        assert_eq!(
            bearer_from_header(Some("Bearer abc.123-_~+/=")),
            Ok("abc.123-_~+/=")
        );
    }

    #[test]
    fn scheme_is_case_insensitive() {
        for h in ["bearer tok", "BEARER tok", "BeArEr tok"] {
            assert_eq!(bearer_from_header(Some(h)), Ok("tok"), "header {h:?}");
        }
    }

    #[test]
    fn missing_header_is_unauthorized() {
        assert_eq!(bearer_from_header(None), Err(AuthError::Unauthorized));
    }

    #[test]
    fn missing_or_empty_token_is_unauthorized() {
        for h in ["Bearer", "Bearer ", "Bearer\ttok"] {
            assert_eq!(
                bearer_from_header(Some(h)),
                Err(AuthError::Unauthorized),
                "header {h:?}"
            );
        }
    }

    #[test]
    fn wrong_scheme_is_unauthorized() {
        for h in ["Basic abc", "Digest abc", "Bearerx abc"] {
            assert_eq!(
                bearer_from_header(Some(h)),
                Err(AuthError::Unauthorized),
                "header {h:?}"
            );
        }
    }

    #[test]
    fn non_b64token_characters_are_unauthorized() {
        // Double space (token starts with a space), embedded space, CR, LF, unicode, non-padding
        // `=`, padding-only — all rejected before any hashing or network work (structurally: the
        // helper is pure, there is nothing to reach).
        for h in [
            "Bearer  tok",
            "Bearer a b",
            "Bearer a\rb",
            "Bearer a\nb",
            "Bearer tök",
            "Bearer a=b",
            "Bearer ===",
        ] {
            assert_eq!(
                bearer_from_header(Some(h)),
                Err(AuthError::Unauthorized),
                "header {h:?}"
            );
        }
    }

    #[test]
    fn trailing_padding_is_accepted() {
        assert_eq!(bearer_from_header(Some("Bearer abc==")), Ok("abc=="));
    }

    #[test]
    fn oversize_token_is_unauthorized() {
        let at_cap = format!("Bearer {}", "a".repeat(8192));
        assert!(bearer_from_header(Some(&at_cap)).is_ok());
        let over_cap = format!("Bearer {}", "a".repeat(8193));
        assert_eq!(
            bearer_from_header(Some(&over_cap)),
            Err(AuthError::Unauthorized)
        );
    }

    #[test]
    fn www_authenticate_challenge_is_byte_constant() {
        // Surfaces must emit exactly this — never anything derived from an `Unavailable` payload.
        assert_eq!(WWW_AUTHENTICATE, "Bearer error=\"invalid_token\"");
        assert_eq!(
            WWW_AUTHENTICATE.as_bytes(),
            b"Bearer error=\"invalid_token\""
        );
    }
}
