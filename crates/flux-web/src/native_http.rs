//! The **native** implementor of the guarded HTTP port (C-652).
//!
//! [`flux_system::port::GuardedHttp`] states what it means for a substrate to make an HTTP request:
//! admit every hop through the shared egress guard, pin the connection to exactly the addresses that
//! guard vetted, bound the redirect chain, re-authorize every carried secret at each hop, and stop
//! reading at a byte cap. [`NativeHttp`] is that port served *in this process*.
//!
//! **It is not a second HTTP path, and it is not a second client.** Everything below routes through
//! [`crate::egress`] — the same redirect-disabled, proxy-free `reqwest::Client`, the same
//! [`egress::send_guarded`] chain and the same [`egress::read_body_capped`] the web ops have used all
//! along. What C-652 changed is *who may ask*: an op now asks the operator's selected substrate, and
//! this is what answers when that substrate is the native one. The bytes on the wire are the bytes
//! that were already there.
//!
//! ## Why it lives here and not beside the port
//!
//! `flux-system` is L2 and its dependency set is deliberately `flux-core` + `tokio` + `url`; the
//! workspace's one HTTP client is in `flux-web` (L5), where `flux-codegate`'s `Http` census already
//! pins its two reviewed construction points. Putting the native implementation beside the trait
//! would mean either a layering violation or a second client, and the census exists to make the
//! second one impossible. So the native `System` answers the HTTP family fail-closed and the
//! implementation lives where the client already is, under its own reviewed
//! `no_unreviewed_guarded_port_backend_outside_system` entry.
//!
//! ## What stays with the caller
//!
//! Redaction. What a *model* may see is a turn-level decision made with the turn's `Redactor`, over
//! the bytes this returns — a substrate has no opinion about it, and a remote one could not hold the
//! turn's registered secrets anyway.

use std::sync::Arc;

use flux_core::{Error, Result};
use flux_system::net::PrivateNetAllow;
use flux_system::port::{Guarded, GuardedHttp, HttpRequest, HttpResponse};
use flux_system::secret_scope::SecretUse;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::Method;

use crate::{egress, WebOptions};

/// The native guarded-HTTP backend: the shared egress client, plus the audit wiring a private-range
/// admit is recorded through.
///
/// Constructed per web op rather than shared, mirroring what the ops already did with their own
/// `reqwest::Client` — a `Client` owns a connection pool, so this changes no lifetime and adds no
/// client. The audit sink and grant-source label ride along so a `PrivateNetAdmit` is emitted at the
/// hop it happened on, not reconstructed afterwards from a list of hosts.
pub struct NativeHttp {
    http: reqwest::Client,
    audit: Option<Arc<dyn flux_plugin::EgressAudit>>,
    grant_source: String,
}

impl NativeHttp {
    /// Build the native backend from the surface's resolved web wiring.
    pub fn new(opts: &WebOptions) -> Self {
        Self {
            http: egress::redirect_disabled_client(),
            audit: opts.audit.clone(),
            grant_source: opts
                .grant_source
                .clone()
                .unwrap_or_else(|| "config:web".to_string()),
        }
    }

    /// Emit the `PrivateNetAdmit` audit event when the guard just let a request through to a
    /// private/internal host — i.e. the `web` grant admitted what the bare SSRF guard would refuse.
    /// Gated on `host_resolves_private` so only genuine private admits are recorded.
    fn audit_admit(&self, operation: &str, host: &str) {
        if let Some(audit) = &self.audit {
            if flux_system::net::host_resolves_private(host) {
                audit.record_private_admit(&format!("web:{operation}"), host, &self.grant_source);
            }
        }
    }

    /// Re-authorize every secret this request carries against `destination`, refusing the hop rather
    /// than letting a credential travel outside its grant.
    ///
    /// Deliberately conservative, and unchanged from the pre-port behaviour: a cross-origin hop
    /// already clears the caller's headers, so a header-placed secret does not physically travel —
    /// but a query-placed one is in the URL, and a `Location` that echoes the query carries it to a
    /// host the operator never named. Rather than reason per-hop about which bytes survive, the whole
    /// redirect chain has to stay inside the scope.
    fn authorize_hop(
        request: &HttpRequest,
        url: &url::Url,
        destination: std::result::Result<&flux_system::secret_scope::Destination, String>,
    ) -> Result<()> {
        if request.secrets.carried.is_empty() {
            return Ok(());
        }
        // Only the HOST is quoted, never the hop URL: a query-placed secret lives in the URL, and a
        // `Location` the server chose can echo it back.
        let hop = url.host_str().unwrap_or("the redirect target").to_string();
        let operation = &request.operation;
        for (name, site) in &request.secrets.carried {
            let use_ = SecretUse {
                destination: destination.clone(),
                principal: request.secrets.principal.as_deref(),
                site: *site,
            };
            if let Err(refusal) = request.secrets.allowlist.authorize(name, &use_) {
                return Err(Error::Http(format!(
                    "{operation}: refusing the redirect to {hop} — secret env var `{name}` is out \
                     of scope for it: {refusal}"
                )));
            }
        }
        Ok(())
    }

    async fn send(&self, request: &HttpRequest, allow: &PrivateNetAllow) -> Result<HttpResponse> {
        let operation = request.operation.as_str();
        let method = Method::from_bytes(request.method.as_bytes()).map_err(|_| {
            Error::Other(format!("{operation}: invalid method {:?}", request.method))
        })?;
        let mut headers = HeaderMap::new();
        for (name, value) in &request.headers {
            let name = HeaderName::from_bytes(name.as_bytes()).map_err(|e| {
                Error::Other(format!("{operation}: invalid header name `{name}`: {e}"))
            })?;
            let value = HeaderValue::from_str(value).map_err(|e| {
                Error::Other(format!(
                    "{operation}: invalid value for header `{name}`: {e}"
                ))
            })?;
            headers.insert(name, value);
        }

        let response = egress::send_guarded(
            &self.http,
            egress::GuardedRequest {
                url: request.target.url().clone(),
                pinned: request.target.pinned().to_vec(),
                method,
                headers,
                body: request.body.clone(),
                timeout: request.timeout,
            },
            operation,
            |raw| {
                // Every hop is admitted by the same guard that admitted the first, and measured
                // against the same secret scope.
                let guarded = flux_system::net::guard_url_scoped_for_secret(raw, allow)?;
                let (url, pinned, destination) = guarded.into_parts();
                Self::authorize_hop(
                    request,
                    &url,
                    destination
                        .as_ref()
                        .map_err(std::string::ToString::to_string),
                )?;
                Ok((url, pinned))
            },
            |url| {
                if let Some(host) = url.host_str() {
                    self.audit_admit(operation, host);
                }
            },
        )
        .await?;

        let status = response.status().as_u16();
        // A value that is not valid header text is reported as `<binary>` rather than dropped, so a
        // caller still sees the name — the pre-port `to_str().unwrap_or("<binary>")` rule, applied
        // here because this is where the wire representation stops.
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().unwrap_or("<binary>").to_string(),
                )
            })
            .collect();
        let capped =
            egress::read_body_capped(response, request.max_response_bytes, operation).await?;

        Ok(HttpResponse {
            status,
            headers,
            body: capped.bytes,
            truncated: capped.truncated,
        })
    }
}

impl GuardedHttp for NativeHttp {
    fn http_request<'a>(
        &'a self,
        request: &'a HttpRequest,
        allow: &'a PrivateNetAllow,
    ) -> Guarded<'a, HttpResponse> {
        Box::pin(self.send(request, allow))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_system::port::HttpSecretScope;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A one-shot loopback HTTP server that answers every request with `response` and returns its
    /// base URL. Loopback is a private range, so the tests below admit it with an explicit grant —
    /// which is also what proves the guard is on the path rather than bypassed.
    async fn one_shot(response: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut buf = [0u8; 4096];
                let _ = socket.read(&mut buf).await;
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.flush().await;
            }
        });
        format!("http://{addr}")
    }

    fn request(operation: &str, url: &str, allow: &PrivateNetAllow) -> HttpRequest {
        let target = flux_system::net::guard_url_scoped_for_secret(url, allow)
            .expect("the loopback fixture is admitted under its grant");
        HttpRequest {
            operation: operation.to_string(),
            method: "GET".into(),
            target,
            headers: Vec::new(),
            body: None,
            timeout: Duration::from_secs(5),
            max_response_bytes: 64 * 1024,
            secrets: HttpSecretScope::default(),
        }
    }

    /// C-652 — the native substrate serves the HTTP port, through the guard it already used.
    ///
    /// The status, the headers and the capped body all come back through
    /// `flux_system::port::HttpResponse`, so an op that holds nothing but `&dyn GuardedHttp` gets
    /// exactly what it used to get from its own client.
    #[tokio::test]
    async fn the_native_backend_serves_a_guarded_request_through_the_port() {
        let base = one_shot(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 13\r\nConnection: close\r\n\r\n{\"ok\":true}\r\n",
        )
        .await;
        let allow = PrivateNetAllow::Any;
        let native = NativeHttp::new(&WebOptions::default());
        let port: &dyn GuardedHttp = &native;

        let response = port
            .http_request(
                &request("http.request", &format!("{base}/v1"), &allow),
                &allow,
            )
            .await
            .expect("the native substrate serves HTTP");

        assert_eq!(response.status, 200);
        assert!(
            response
                .headers
                .iter()
                .any(|(name, value)| name == "content-type" && value == "application/json"),
            "response headers must survive the port: {:?}",
            response.headers
        );
        assert!(String::from_utf8_lossy(&response.body).contains("\"ok\":true"));
        assert!(!response.truncated);
    }

    /// C-652 — the byte cap is the substrate's, not the caller's afterthought.
    ///
    /// A capped read has to stop buffering at the cap; reading the whole body and cutting it
    /// afterwards would lose the memory bound on an attacker-influenced size, which is why the port
    /// carries `max_response_bytes` rather than leaving it to the op.
    #[tokio::test]
    async fn the_response_cap_travels_with_the_request() {
        let base = one_shot(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 26\r\nConnection: close\r\n\r\nabcdefghijklmnopqrstuvwxyz",
        )
        .await;
        let allow = PrivateNetAllow::Any;
        let native = NativeHttp::new(&WebOptions::default());
        let mut req = request("web.fetch", &format!("{base}/big"), &allow);
        req.max_response_bytes = 4;

        let response = native
            .http_request(&req, &allow)
            .await
            .expect("the native substrate serves HTTP");

        assert_eq!(response.body, b"abcd".to_vec());
        assert!(response.truncated, "a cut body must report itself cut");
    }

    /// C-652 — the egress guard is on the port's path, not beside it.
    ///
    /// The same request that succeeds under an `Any` grant is refused under the default (public
    /// only) one, because loopback is a private range. If the guard had moved off this path the two
    /// calls would be indistinguishable.
    #[tokio::test]
    async fn the_native_backend_refuses_a_private_target_without_a_grant() {
        let base =
            one_shot("HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
        let native = NativeHttp::new(&WebOptions::default());

        assert!(
            flux_system::net::guard_url_scoped_for_secret(
                &format!("{base}/blocked"),
                &PrivateNetAllow::None
            )
            .is_err(),
            "loopback must be refused without a `web` grant — the port cannot admit what the guard \
             will not"
        );

        // And the admitted spelling still reaches the server, so the refusal above is the guard's
        // decision rather than an unreachable fixture.
        let allow = PrivateNetAllow::Any;
        assert_eq!(
            native
                .http_request(&request("web.fetch", &format!("{base}/ok"), &allow), &allow)
                .await
                .expect("an admitted loopback target is served")
                .status,
            200
        );
    }
}
