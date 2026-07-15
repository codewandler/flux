//! Shared redirect and response-body handling for the native HTTP tools.
//!
//! URL admission remains owned by `flux-system`; this module only makes sure that admission is
//! repeated for every redirect hop and that reqwest never follows a hop behind the guard's back.

use std::net::SocketAddr;
use std::time::Duration;

use flux_core::{Error, Result};
use reqwest::header::{HeaderMap, LOCATION};
use reqwest::{Client, Method, Response, StatusCode};
use url::Url;

/// Maximum redirects followed by one tool invocation.
pub(crate) const MAX_REDIRECTS: usize = 5;

/// A reqwest client whose redirect policy is deliberately inert. Every redirect is followed by
/// [`send_guarded`] only after the shared flux-system egress guard admits its target.
pub(crate) fn redirect_disabled_client() -> Client {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("the redirect-disabled reqwest client uses only static options")
}

/// Send one request and manually follow a bounded redirect chain.
///
/// Only GET and HEAD are followed. A caller-supplied GET body is sent to the first URL but is not
/// replayed to a redirect target. Every target is passed through `guard`; HTTPS→HTTP downgrades are
/// refused. Caller headers survive same-origin hops, while a cross-origin hop starts with an empty
/// header map so custom/API-key headers cannot become ambient credentials for another origin.
#[allow(clippy::too_many_arguments)]
pub(crate) struct GuardedRequest {
    pub(crate) url: Url,
    /// The socket addresses the guard vetted for `url`'s host — the connection is pinned to exactly
    /// these so reqwest cannot re-resolve to a different (internal) address at connect (C-77). Empty
    /// means the guard resolved (and therefore vetted) NOTHING, which is refused rather than connected
    /// unpinned — see [`pinned_client`].
    pub(crate) pinned: Vec<SocketAddr>,
    pub(crate) method: Method,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Option<Vec<u8>>,
    pub(crate) timeout: Duration,
}

pub(crate) async fn send_guarded<G, A>(
    client: &Client,
    request: GuardedRequest,
    op: &str,
    mut guard: G,
    mut on_admit: A,
) -> Result<Response>
where
    G: FnMut(&str) -> Result<(Url, Vec<SocketAddr>)>,
    A: FnMut(&Url),
{
    let deadline = tokio::time::Instant::now() + request.timeout;
    let follows_redirects = request.method == Method::GET || request.method == Method::HEAD;
    let method = request.method;
    let mut url = request.url;
    let mut pinned = request.pinned;
    let mut headers = request.headers;
    let mut body = request.body;
    let mut redirects = 0usize;

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(Error::Http(format!("{op}: request timed out")));
        }

        // Pin this hop to the guard's vetted addresses so the connection can't be rebound to an
        // internal host between admission and connect.
        let hop = pinned_client(client, &url, &pinned, op)?;
        let mut request = hop
            .request(method.clone(), url.clone())
            .headers(headers.clone())
            .timeout(remaining);
        if let Some(bytes) = &body {
            request = request.body(bytes.clone());
        }
        let response = request
            .send()
            .await
            .map_err(|e| Error::Http(format!("{op}: {e}")))?;
        on_admit(&url);

        if !follows_redirects || !is_followed_redirect(response.status()) {
            return Ok(response);
        }
        let Some(location) = response.headers().get(LOCATION) else {
            return Ok(response);
        };
        if redirects == MAX_REDIRECTS {
            return Err(Error::Http(format!(
                "{op}: too many redirects (maximum {MAX_REDIRECTS})"
            )));
        }
        let location = location
            .to_str()
            .map_err(|_| Error::Http(format!("{op}: redirect Location is not valid text")))?;
        let joined = url
            .join(location)
            .map_err(|e| Error::Http(format!("{op}: invalid redirect Location: {e}")))?;
        let (next, next_pinned) = guard(joined.as_str())?;
        if url.scheme() == "https" && next.scheme() == "http" {
            return Err(Error::Http(format!(
                "{op}: refusing HTTPS-to-HTTP redirect to {next}"
            )));
        }
        if !same_origin(&url, &next) {
            headers.clear();
        }

        // Even a GET body can contain credentials. It reaches only the URL the caller named and is
        // never replayed by redirect handling.
        body = None;
        url = next;
        pinned = next_pinned;
        redirects += 1;
    }
}

/// Build the client for one redirect hop. When the guard vetted concrete addresses, pin the URL's
/// host to exactly them via [`reqwest::ClientBuilder::resolve_to_addrs`] so reqwest connects only
/// there — no connect-time re-resolution, closing the DNS-rebinding TOCTOU (C-77). With NO vetted
/// addresses the guard vetted nothing at all, so this fails closed instead of connecting unpinned.
/// The shared client's redirect policy is inert, so the fresh per-hop client mirrors it.
fn pinned_client(shared: &Client, url: &Url, pinned: &[SocketAddr], op: &str) -> Result<Client> {
    // No host at all can't reach here (the guard rejects it), but stay defensive.
    let Some(host) = url.host_str() else {
        return Ok(shared.clone());
    };
    // An EMPTY pin set means the guard resolved NOTHING for this host — its `block_if` vetting loop
    // never ran (see `guard_and_pin`). Falling back to an unpinned client here would let connect-time
    // DNS reach an address the guard never approved: an attacker who SERVFAILs the guard's query and
    // then answers `169.254.169.254` at connect would bypass the pin entirely, on exactly the path the
    // pin exists to protect. Fail closed. IP-literal hosts always carry a pin, so this refuses only
    // unresolvable domains — which could not have connected anyway.
    if pinned.is_empty() {
        return Err(Error::Http(format!(
            "{op}: refusing to connect to {host} — the egress guard could not resolve it to a vetted \
             address, so the connection would re-resolve at connect time (DNS rebinding)"
        )));
    }
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .resolve_to_addrs(host, pinned)
        .build()
        .map_err(|e| Error::Http(format!("{op}: building a pinned client failed: {e}")))
}

fn is_followed_redirect(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::MOVED_PERMANENTLY
            | StatusCode::FOUND
            | StatusCode::SEE_OTHER
            | StatusCode::TEMPORARY_REDIRECT
            | StatusCode::PERMANENT_REDIRECT
    )
}

fn same_origin(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str()
            .zip(b.host_str())
            .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b))
        && a.port_or_known_default() == b.port_or_known_default()
}

/// A response body retained only up to the caller's byte budget.
pub(crate) struct CappedBody {
    pub(crate) bytes: Vec<u8>,
    pub(crate) truncated: bool,
}

/// Incrementally read at most `max` response bytes. Dropping the response after the cap is reached
/// stops buffering immediately; no whole-body `bytes()`/`text()` allocation happens first.
pub(crate) async fn read_body_capped(
    mut response: Response,
    max: usize,
    op: &str,
) -> Result<CappedBody> {
    let declared_over_cap = response
        .content_length()
        .is_some_and(|len| len > max as u64);
    let mut bytes = Vec::with_capacity(
        response
            .content_length()
            .map(|len| len.min(max as u64) as usize)
            .unwrap_or(0),
    );
    let mut truncated = false;

    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| Error::Http(format!("{op}: response body read failed: {e}")))?
    {
        let remaining = max.saturating_sub(bytes.len());
        if remaining == 0 {
            truncated = true;
            break;
        }
        let take = remaining.min(chunk.len());
        bytes.extend_from_slice(&chunk[..take]);
        if take < chunk.len() || (declared_over_cap && bytes.len() == max) {
            truncated = true;
            break;
        }
    }

    Ok(CappedBody { bytes, truncated })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// C-77 (review follow-up): an empty pin set means the guard vetted NOTHING (its resolve failed),
    /// so connecting would re-resolve at connect time — the exact DNS-rebinding hole the pin closes.
    /// It must fail closed rather than silently fall back to an unpinned client.
    #[test]
    fn empty_pin_refuses_instead_of_connecting_unpinned() {
        let shared = redirect_disabled_client();
        let url = Url::parse("https://rebinding.example/hook").unwrap();
        let err = pinned_client(&shared, &url, &[], "web.fetch")
            .expect_err("an unvetted (unresolvable) host must not connect unpinned");
        let msg = err.to_string();
        assert!(
            msg.contains("rebinding.example") && msg.contains("vetted"),
            "the refusal names the unvetted host: {msg}"
        );
    }

    /// The vetted addresses still build a pinned client (the guard's answer is honored).
    #[test]
    fn vetted_addresses_build_a_pinned_client() {
        let shared = redirect_disabled_client();
        let url = Url::parse("https://rebinding.example/hook").unwrap();
        let pin = vec![SocketAddr::new("93.184.216.34".parse().unwrap(), 443)];
        assert!(pinned_client(&shared, &url, &pin, "web.fetch").is_ok());
    }
}
