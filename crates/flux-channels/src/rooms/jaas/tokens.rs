//! [`BraveTalkTokens`] — the vendor implementation of [`JaasTokens`]: three HTTP requests that turn
//! a room name into a guest JWT and a MUC JID.
//!
//! ## Everything here is a recorded observation, not a published contract
//!
//! **Brave publishes no API for this.** Every request shape below was derived from the open-source
//! [`brave/brave-talk`](https://github.com/brave/brave-talk) client and confirmed against the live
//! service once, on **2026-07-30** (`docs/designs/meeting-rooms.md`, "Feasibility"). Each assumption
//! that would break silently if the vendor changed is marked `VENDOR ASSUMPTION` at the line that
//! depends on it, so a future failure is diagnosable as *the vendor moved* rather than *our code is
//! wrong*. If several of them fail at once, re-derive the handshake from the client rather than
//! patching them one at a time.
//!
//! ## The handshake
//!
//! ```text
//! OPTIONS <token_service>/api/v1/rooms/<room>   → x-csrf-token: …  + Set-Cookie: _gorilla_csrf=…
//! PUT     <token_service>/api/v1/rooms/<room>   → 200 {"jwt": "…"}
//!         x-csrf-token: …, Cookie: …, body {"mauP": true}
//! POST    <conference_service>/<tenant>/conference-request/v1?room=<room>
//!         Authorization: Bearer <jwt>          → 200 {"ready": true, "room": "<lowercased>@…"}
//! ```
//!
//! `PUT` **joins** an existing room; `POST` on the same path *creates* one and needs a Brave Premium
//! subscriber cookie. This backend only ever joins — see the own-room scoping in the parent module.
//!
//! ## Safety
//!
//! - **Every request is pinned to the addresses the egress guard vetted.** `guard_url_scoped_pinned`
//!   resolves and vets, and the client is built with `resolve_to_addrs` + `no_proxy` so the
//!   connection cannot be re-resolved to an internal address between admission and connect. An empty
//!   pin set fails closed. Redirects are refused outright: a redirect would move the request off the
//!   vetted origin, and it would carry the `Authorization` header with it. This mirrors
//!   `flux-web`'s crawler; there is no second URL guard here.
//! - **No response body ever reaches an error message.** A failing vendor response can echo the
//!   token or the CSRF value back at us, so failures name the status and the query-trimmed URL and
//!   nothing else.

use std::net::SocketAddr;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE, COOKIE, SET_COOKIE};
use reqwest::{Client, Method, Response, StatusCode};
use serde::Deserialize;
use url::Url;

use flux_core::{Error, Result};
use flux_system::net::{guard_url_scoped_pinned, PrivateNetAllow};

use super::{Conference, GuestToken, JaasTokens};
use crate::rooms::xmpp::endpoint_for_display;

/// Brave's token service. Public and unauthenticated — joining needs no account.
pub const DEFAULT_BRAVE_TOKEN_SERVICE: &str = "https://talk.brave.com";
/// The 8x8 JaaS API that allocates conference focus.
pub const DEFAULT_JAAS_CONFERENCE_SERVICE: &str = "https://8x8.vc";

/// Per-request budget. Three requests stand between a program starting and an agent being in the
/// room, so this is short enough that a wedged vendor surfaces as a failed join rather than a hang.
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

/// How many times focus allocation is re-asked while it answers `ready: false`.
const CONFERENCE_READY_ATTEMPTS: usize = 5;

/// How long to wait between those attempts.
const CONFERENCE_READY_BACKOFF: Duration = Duration::from_millis(500);

/// The largest vendor response this backend will read. A guest JWT is ~1 KB; anything at this size
/// is not an answer to any of these three requests.
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

/// VENDOR ASSUMPTION (2026-07-30): the CSRF token comes back in this response header, and the same
/// value must be echoed on the `PUT`. Gorilla's default header name.
const CSRF_HEADER: &str = "x-csrf-token";

/// The [`JaasTokens`] implementation for Brave Talk and any 8x8 JaaS tenant that mints guest tokens
/// the same way.
///
/// Holds no credential: the guest path is unauthenticated, which is exactly why the CSRF handshake
/// exists. An own-tenant source that signs its own JWT from an operator's API key is a *second*
/// implementation of [`JaasTokens`] and changes nothing else — it is deferred (it needs an RS256
/// signer this workspace does not carry).
#[derive(Debug, Clone)]
pub struct BraveTalkTokens {
    token_service: String,
    conference_service: String,
    private_net: PrivateNetAllow,
}

impl Default for BraveTalkTokens {
    fn default() -> Self {
        Self::new()
    }
}

impl BraveTalkTokens {
    /// Brave's public token service and 8x8's conference service, with the egress guard fully on.
    pub fn new() -> Self {
        Self {
            token_service: DEFAULT_BRAVE_TOKEN_SERVICE.to_string(),
            conference_service: DEFAULT_JAAS_CONFERENCE_SERVICE.to_string(),
            private_net: PrivateNetAllow::None,
        }
    }

    /// Point token minting somewhere other than `talk.brave.com` — another JaaS front end, or a
    /// test double.
    pub fn token_service(mut self, base: impl Into<String>) -> Self {
        self.token_service = base.into();
        self
    }

    /// Point focus allocation somewhere other than `8x8.vc`.
    pub fn conference_service(mut self, base: impl Into<String>) -> Self {
        self.conference_service = base.into();
        self
    }

    /// Allow these endpoints to resolve to a private/loopback address. The egress guard's scoped
    /// grant, not a bypass of it.
    pub fn allow_private_net(mut self, allow: bool) -> Self {
        self.private_net = PrivateNetAllow::from_legacy_bool(allow);
        self
    }

    /// `<token_service>/api/v1/rooms/<room>`, with the room percent-encoded as one path segment.
    fn room_url(&self, room: &str) -> Result<Url> {
        let mut url = parse_base(&self.token_service, "token service")?;
        url.path_segments_mut()
            .map_err(|_| Error::Other("jaas: the token service base cannot carry a path".into()))?
            .pop_if_empty()
            .extend(["api", "v1", "rooms", room]);
        Ok(url)
    }

    /// `<conference_service>/<tenant>/conference-request/v1?room=<room>`.
    fn conference_url(&self, tenant: &str, room: &str) -> Result<Url> {
        let mut url = parse_base(&self.conference_service, "conference service")?;
        url.path_segments_mut()
            .map_err(|_| {
                Error::Other("jaas: the conference service base cannot carry a path".into())
            })?
            .pop_if_empty()
            .extend([tenant, "conference-request", "v1"]);
        url.query_pairs_mut().append_pair("room", room);
        Ok(url)
    }

    /// Send one guarded, pinned request. `what` names the step in any error.
    async fn send(
        &self,
        method: Method,
        url: &Url,
        headers: HeaderMap,
        body: Option<Vec<u8>>,
        what: &str,
    ) -> Result<Response> {
        // Vet and capture the addresses to pin to. This resolve is the one the connection uses, so
        // there is no rebinding gap between admission and connect (C-77).
        let (url, pinned) = guard_url_scoped_pinned(url.as_str(), &self.private_net)?;
        let client = pinned_client(&url, &pinned, what)?;
        let mut request = client
            .request(method, url.clone())
            .headers(headers)
            .timeout(HTTP_TIMEOUT);
        if let Some(bytes) = body {
            request = request.body(bytes);
        }
        request.send().await.map_err(|e| {
            // Query-trimmed: a conference-request URL carries the room, and the endpoint is the one
            // thing worth naming. The error from reqwest never carries our headers.
            Error::Http(format!(
                "jaas: {what} against {} failed: {e}",
                endpoint_for_display(url.as_str())
            ))
        })
    }

    /// Turn a non-success status into an error that names the step and the status — and nothing
    /// from the body, which a vendor can fill with anything, including our own token echoed back.
    fn expect_ok(&self, response: &Response, what: &str, hint: &str) -> Result<()> {
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        Err(Error::Http(format!(
            "jaas: {what} against {} answered HTTP {} ({}) — {hint}",
            endpoint_for_display(response.url().as_str()),
            status.as_u16(),
            status.canonical_reason().unwrap_or("unrecognized status"),
        )))
    }
}

#[async_trait]
impl JaasTokens for BraveTalkTokens {
    /// `OPTIONS` for the CSRF token and its cookie, then `PUT` to join and receive the guest JWT.
    async fn guest_token(&self, room: &str) -> Result<GuestToken> {
        let url = self.room_url(room)?;

        let preflight = self
            .send(
                Method::OPTIONS,
                &url,
                HeaderMap::new(),
                None,
                "the CSRF preflight",
            )
            .await?;
        self.expect_ok(
            &preflight,
            "the CSRF preflight",
            "the token service refused the preflight; the handshake may have changed",
        )?;
        // VENDOR ASSUMPTION (2026-07-30): the preflight carries both halves of the CSRF pair — the
        // token in a response header and a cookie to send back with it. Either one missing means
        // the `PUT` below is going to be refused, so fail here where the cause is still legible.
        let csrf = preflight
            .headers()
            .get(CSRF_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
            .ok_or_else(|| {
                Error::Http(format!(
                    "jaas: the CSRF preflight returned no `{CSRF_HEADER}` header — the token \
                     service's handshake has changed"
                ))
            })?;
        let cookies = cookie_jar(&preflight);
        if cookies.is_empty() {
            return Err(Error::Http(
                "jaas: the CSRF preflight set no cookie — the token service's handshake has changed"
                    .into(),
            ));
        }

        let mut headers = HeaderMap::new();
        headers.insert(
            CSRF_HEADER,
            HeaderValue::from_str(&csrf).map_err(|_| {
                Error::Http("jaas: the CSRF token is not a valid header value".into())
            })?,
        );
        headers.insert(
            COOKIE,
            HeaderValue::from_str(&cookies)
                .map_err(|_| Error::Http("jaas: the CSRF cookie is not a valid header".into()))?,
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        // VENDOR ASSUMPTION (2026-07-30): `PUT` *joins* an existing room and needs nothing but the
        // CSRF pair. `POST` on this path creates a room and requires a Brave Premium subscriber
        // cookie — this backend must never send one (own-room scope, see the parent module).
        let minted = self
            .send(
                Method::PUT,
                &url,
                headers,
                // VENDOR ASSUMPTION (2026-07-30): the body the open-source client sends.
                Some(br#"{"mauP": true}"#.to_vec()),
                "the guest-token request",
            )
            .await?;
        self.expect_ok(
            &minted,
            "the guest-token request",
            "the CSRF pair was rejected, or the room does not exist",
        )?;

        #[derive(Deserialize)]
        struct Minted {
            jwt: String,
        }
        // VENDOR ASSUMPTION (2026-07-30): the JWT arrives as the `jwt` field of a JSON object.
        let body: Minted = read_json(minted, "the guest-token request").await?;
        GuestToken::parse(body.jwt)
    }

    /// `POST /<tenant>/conference-request/v1` with the JWT as Bearer, re-asking while focus is still
    /// allocating. The MUC JID comes from this response and from nowhere else.
    async fn conference(&self, room: &str, token: &GuestToken) -> Result<Conference> {
        let url = self.conference_url(token.tenant(), room)?;
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            // The one place the JWT is written into a header. `set_sensitive` keeps it out of
            // reqwest's own `Debug` rendering of the header map.
            {
                let mut value = HeaderValue::from_str(&format!("Bearer {}", token.jwt()))
                    .map_err(|_| Error::Http("jaas: the guest token is not header-safe".into()))?;
                value.set_sensitive(true);
                value
            },
        );

        for attempt in 1..=CONFERENCE_READY_ATTEMPTS {
            let response = self
                .send(
                    Method::POST,
                    &url,
                    headers.clone(),
                    // VENDOR ASSUMPTION (2026-07-30): the room rides the query string and the
                    // request carries no body. Observed answering 200.
                    None,
                    "focus allocation",
                )
                .await?;
            if response.status() == StatusCode::UNAUTHORIZED
                || response.status() == StatusCode::FORBIDDEN
            {
                return Err(Error::Http(format!(
                    "jaas: focus allocation against {} rejected the guest token (HTTP {}) — it may \
                     have expired, or the tenant does not match",
                    endpoint_for_display(response.url().as_str()),
                    response.status().as_u16()
                )));
            }
            self.expect_ok(
                &response,
                "focus allocation",
                "the conference-request API refused; the handshake may have changed",
            )?;

            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Allocated {
                // `Option`, not a defaulted `bool`: a response that *omits* `ready` is a changed
                // response shape, and must say so rather than masquerade as "not ready yet" and
                // burn the retry budget on a diagnosis that points at the wrong thing.
                ready: Option<bool>,
                #[serde(default)]
                room: Option<String>,
                #[serde(default)]
                focus_jid: Option<String>,
            }
            let body: Allocated = read_json(response, "focus allocation").await?;
            let ready = body.ready.ok_or_else(|| {
                Error::Http(
                    "jaas: focus allocation answered without a `ready` field — the vendor's \
                     response shape has changed"
                        .into(),
                )
            })?;

            // VENDOR ASSUMPTION, and the *inferred* one: the spike only ever observed
            // `ready: true`. `ready: false` is Jitsi's documented "focus is still allocating"
            // state, so it is retried on a fixed backoff rather than keyed on a response field
            // this repo has never seen.
            if !ready {
                if attempt == CONFERENCE_READY_ATTEMPTS {
                    return Err(Error::Http(format!(
                        "jaas: focus allocation still reported the conference as not ready after \
                         {CONFERENCE_READY_ATTEMPTS} attempts"
                    )));
                }
                tokio::time::sleep(CONFERENCE_READY_BACKOFF).await;
                continue;
            }

            // The whole reason this call exists on the join path: the response lowercases the room
            // name and the JWT's `room` claim does not. Never rebuild this locally.
            let room_jid = body.room.ok_or_else(|| {
                Error::Http(
                    "jaas: focus allocation reported ready but named no room — the MUC JID must \
                     come from this response, and there is nothing safe to substitute for it"
                        .into(),
                )
            })?;
            return Ok(Conference {
                room_jid,
                focus_jid: body.focus_jid,
            });
        }
        unreachable!("the loop returns or errors on its last attempt")
    }
}

/// Parse a configured base URL, refusing anything the guard would not accept anyway.
fn parse_base(raw: &str, what: &str) -> Result<Url> {
    let url = Url::parse(raw.trim_end_matches('/'))
        .map_err(|e| Error::Other(format!("jaas: bad {what} base `{raw}`: {e}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(Error::Other(format!(
            "jaas: the {what} base must be an http(s) url, got `{}`",
            url.scheme()
        )));
    }
    Ok(url)
}

/// The `Cookie` header to send back, built from the response's `Set-Cookie` headers.
///
/// A deliberately minimal jar: this is two requests against one host inside one function, so
/// attributes (`Path`, `Domain`, `Max-Age`) have nothing to scope and are dropped. It is not a
/// general cookie store and must not become one — if a future step needs cross-request cookie
/// semantics, take reqwest's `cookies` feature instead of growing this.
fn cookie_jar(response: &Response) -> String {
    response
        .headers()
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|v| v.split(';').next())
        .map(str::trim)
        .filter(|pair| !pair.is_empty())
        .collect::<Vec<_>>()
        .join("; ")
}

/// Read a JSON body, **incrementally** capped at [`MAX_RESPONSE_BYTES`].
///
/// Chunk by chunk, refusing as soon as the cap is passed — no whole-body `bytes()`/`text()`
/// allocation happens first, so a hostile or broken vendor cannot make flux buffer an arbitrary
/// response before the limit is noticed. Same posture as `flux-web`'s `read_body_capped`.
///
/// The body is never quoted into an error: a vendor response can contain our own token echoed back.
async fn read_json<T: serde::de::DeserializeOwned>(
    mut response: Response,
    what: &str,
) -> Result<T> {
    let url = endpoint_for_display(response.url().as_str()).to_string();
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| Error::Http(format!("jaas: reading {what} from {url} failed: {e}")))?
    {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(Error::Http(format!(
                "jaas: {what} from {url} answered more than the {MAX_RESPONSE_BYTES}-byte cap"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|e| {
        // `e` names the offending field and offset, never the input.
        Error::Http(format!(
            "jaas: {what} from {url} did not answer the expected JSON ({e}) — the vendor's \
             response shape has changed"
        ))
    })
}

/// A client pinned to the addresses the egress guard vetted.
///
/// Mirrors `flux-web`'s `pinned_client` (C-77), including its fail-closed treatment of an empty pin
/// set: that means the guard resolved *nothing*, so its vetting loop never ran, and an unpinned
/// connection would let connect-time DNS reach an address the guard never approved.
fn pinned_client(url: &Url, pinned: &[SocketAddr], what: &str) -> Result<Client> {
    let Some(host) = url.host_str() else {
        return Err(Error::Http(format!(
            "jaas: {what} has no host to connect to"
        )));
    };
    if pinned.is_empty() {
        return Err(Error::Http(format!(
            "jaas: refusing to connect to {host} for {what} — the egress guard could not resolve \
             it to a vetted address, so the connection would re-resolve at connect time (DNS \
             rebinding)"
        )));
    }
    // flux-allow-direct-io: reviewed HTTP client pinned to the exact addresses admitted by
    // flux-system's egress guard immediately above; this is the network boundary implementation
    // itself, and the same shape flux-web's crawler uses.
    Client::builder()
        // A redirect would move the request off the vetted origin — and carry the `Authorization`
        // header there with it. There is no legitimate redirect in this handshake.
        .redirect(reqwest::redirect::Policy::none())
        // Ambient proxy variables would route the request to an unvetted peer and move DNS
        // resolution behind it, defeating the pin.
        .no_proxy()
        .resolve_to_addrs(host, pinned)
        .build()
        .map_err(|e| {
            Error::Http(format!(
                "jaas: building a pinned client for {what} failed: {e}"
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_room_becomes_one_percent_encoded_path_segment() {
        let tokens = BraveTalkTokens::new();
        assert_eq!(
            tokens.room_url("StandUp").unwrap().as_str(),
            "https://talk.brave.com/api/v1/rooms/StandUp"
        );
        // A room name is untrusted-ish text from configuration; it must not be able to climb the
        // path or add a query.
        assert_eq!(
            tokens.room_url("../../admin?x=1").unwrap().as_str(),
            "https://talk.brave.com/api/v1/rooms/..%2F..%2Fadmin%3Fx=1"
        );
    }

    #[test]
    fn the_conference_url_carries_the_tenant_and_the_room() {
        let url = BraveTalkTokens::new()
            .conference_url("vpaas-magic-cookie-a4818bd", "StandUp")
            .unwrap();
        assert_eq!(
            url.as_str(),
            "https://8x8.vc/vpaas-magic-cookie-a4818bd/conference-request/v1?room=StandUp"
        );
    }

    #[test]
    fn a_base_with_a_trailing_slash_or_a_bad_scheme_is_handled_at_the_edge() {
        assert_eq!(
            BraveTalkTokens::new()
                .token_service("https://talk.brave.com/")
                .room_url("r")
                .unwrap()
                .as_str(),
            "https://talk.brave.com/api/v1/rooms/r"
        );
        let bad = BraveTalkTokens::new()
            .token_service("wss://talk.brave.com")
            .room_url("r");
        assert!(bad.is_err(), "{bad:?}");
    }

    #[test]
    fn the_egress_guard_is_on_unless_it_is_explicitly_relaxed() {
        assert_eq!(BraveTalkTokens::new().private_net, PrivateNetAllow::None);
        assert_eq!(
            BraveTalkTokens::new().allow_private_net(true).private_net,
            PrivateNetAllow::Any
        );
    }

    #[test]
    fn an_unresolvable_host_fails_closed_rather_than_connecting_unpinned() {
        let url = Url::parse("https://example.invalid/x").unwrap();
        let refused = pinned_client(&url, &[], "a test");
        let message = refused
            .expect_err("an empty pin set must fail closed")
            .to_string();
        assert!(message.contains("rebinding"), "{message}");
    }
}
