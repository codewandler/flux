//! Shared redirect and response-body handling for the native HTTP tools.
//!
//! URL admission remains owned by `flux-system`; this module only makes sure that admission is
//! repeated for every redirect hop and that reqwest never follows a hop behind the guard's back.

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
    G: FnMut(&str) -> Result<Url>,
    A: FnMut(&Url),
{
    let deadline = tokio::time::Instant::now() + request.timeout;
    let follows_redirects = request.method == Method::GET || request.method == Method::HEAD;
    let method = request.method;
    let mut url = request.url;
    let mut headers = request.headers;
    let mut body = request.body;
    let mut redirects = 0usize;

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(Error::Http(format!("{op}: request timed out")));
        }

        let mut request = client
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
        let next = guard(joined.as_str())?;
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
        redirects += 1;
    }
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
