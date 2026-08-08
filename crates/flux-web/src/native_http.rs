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
//! ## How a selected native substrate reaches it (C-675)
//!
//! Downward, never upward. A [`NativeHttp`] built here is **attached** to the `System` a selection
//! is composed from (`flux_system::System::with_http`) by the surface that holds both — `flux-cli`,
//! at selection-install time — and the substrate serves the family by delegating to that system.
//! `flux-system` still names no client and gains no dependency: what crosses is an implementation
//! of its own `GuardedHttp`. So a sandboxed selection makes its requests through this file, with
//! its own audit sink, and a substrate nobody composed one onto still refuses.
//!
//! ## What stays with the caller
//!
//! Redaction. What a *model* may see is a turn-level decision made with the turn's `Redactor`, over
//! the bytes this returns — a substrate has no opinion about it, and a remote one could not hold the
//! turn's registered secrets anyway.

use std::sync::Arc;
use std::time::Duration;

use flux_core::{Error, Result};
use flux_system::net::PrivateNetAllow;
use flux_system::port::{Guarded, GuardedHttp, HttpRequest, HttpResponse, PrivateAdmit};
use flux_system::secret_scope::SecretUse;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::Method;

use crate::{egress, retry, WebOptions};

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

    /// Record a private-destination admission: on this backend's own audit sink, **and** in the
    /// answer, returning the admit so it can travel with the response.
    ///
    /// Both, not either. The sink is where an unselected run's `PrivateNetAdmit` has always landed
    /// and it stays there. Reporting the admit as well is what keeps the event visible when this
    /// backend is on the far side of a hop (C-674): the operator reading the turn's audit trail is
    /// not on this machine, and a security event that lands only in a daemon's sink is a security
    /// event nobody sees. Gated on `host_resolves_private` so only genuine private admits count.
    fn audit_admit(&self, operation: &str, host: &str) -> Option<PrivateAdmit> {
        if !flux_system::net::host_resolves_private(host) {
            return None;
        }
        if let Some(audit) = &self.audit {
            audit.record_private_admit(&format!("web:{operation}"), host, &self.grant_source);
        }
        Some(PrivateAdmit {
            host: host.to_string(),
            grant_source: self.grant_source.clone(),
            // This process admitted it, so there is no other substrate to name. A hop across a
            // trust boundary stamps its own kind on the way through.
            substrate: None,
        })
    }

    /// Put the private-destination admissions a **remote** substrate reported onto this process's
    /// audit sink, stamped with the substrate they happened on (C-674).
    ///
    /// Only the reported ones: an admit with no `substrate` happened here, and
    /// [`audit_admit`](Self::audit_admit) already recorded it at the hop. Emitting it a second time
    /// would double-count the security event that matters most.
    ///
    /// The grant is still named — it is the operator's own — with the substrate appended, so an
    /// operator reading the trail sees both *why* an internal host was reachable and *where* the
    /// request that reached it actually ran.
    pub fn record_reported_admits(&self, operation: &str, admits: &[PrivateAdmit]) {
        let Some(audit) = &self.audit else {
            return;
        };
        for admit in admits {
            let Some(substrate) = &admit.substrate else {
                continue;
            };
            audit.record_private_admit(
                &format!("web:{operation}"),
                &admit.host,
                &format!("{} on substrate {substrate}", admit.grant_source),
            );
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
    ///
    /// Two things changed with C-674, both because the caller may now be another process:
    ///
    /// - What is authorized is [`HttpRequest::carried_secrets`], not the caller's `carried` list
    ///   alone. A header whose value materializes a `$secret` says so in the header itself, so a
    ///   credential that is physically on the request is authorized whether or not a separate list
    ///   remembered it.
    /// - It runs on the **first** hop as well as the redirects — see [`NativeHttp::send`].
    fn authorize_hop(
        request: &HttpRequest,
        url: &url::Url,
        destination: std::result::Result<&flux_system::secret_scope::Destination, String>,
        what: &str,
    ) -> Result<()> {
        let carried = request.carried_secrets();
        if carried.is_empty() {
            return Ok(());
        }
        // Only the HOST is quoted, never the hop URL: a query-placed secret lives in the URL, and a
        // `Location` the server chose can echo it back.
        let hop = url.host_str().unwrap_or("an unnamed host").to_string();
        let operation = &request.operation;
        for (name, site) in &carried {
            let use_ = SecretUse {
                destination: destination.clone(),
                principal: request.secrets.principal.as_deref(),
                site: *site,
            };
            if let Err(refusal) = request.secrets.allowlist.authorize(name, &use_) {
                return Err(Error::Http(format!(
                    "{operation}: refusing {what} {hop} — secret env var `{name}` is out of scope \
                     for it: {refusal}"
                )));
            }
        }
        Ok(())
    }

    /// Send one guarded request, riding out a rate limit if the far side asks for one (C-701).
    ///
    /// The loop is here rather than inside [`egress::send_guarded`] for one structural reason: the
    /// first hop's secret re-authorization happens in [`Self::attempt`], *above* the redirect chain.
    /// A retry loop wrapped around `send_guarded` would leave that check outside itself, so every
    /// retry would be riding on the decision the first attempt made — exactly what a retry may not
    /// do. Wrapping the whole attempt instead means a retry re-mints the guarded target, re-admits
    /// it, re-authorizes every carried secret against the destination that admission produced, and
    /// only then re-enters the redirect chain, which re-admits and re-authorizes each hop as it
    /// always has.
    ///
    /// It is also why the retry sits in the *egress client* rather than in an op: the substrate that
    /// sends is the substrate that waits. A selected remote substrate runs this loop next to the
    /// service it is calling, and the wire sees one request and one answer — no new frame, no round
    /// trip per attempt, and no coordinator holding a link open per retry.
    async fn send(&self, request: &HttpRequest, allow: &PrivateNetAllow) -> Result<HttpResponse> {
        let deadline = tokio::time::Instant::now() + request.timeout;
        // Collected across attempts, not just the last one: an admission made on a retried attempt
        // is exactly as real a security event as one made on the first, and for a caller across a
        // hop this report is the only place it can appear.
        let mut admits: Vec<PrivateAdmit> = Vec::new();
        let mut report = flux_system::port::RetryReport::default();

        loop {
            // Every attempt after the first re-runs the egress guard on the URL the caller named,
            // rather than re-using the addresses an earlier attempt was vetted for. It costs one
            // resolve on a path that is already waiting seconds, and it is what "exactly as a first
            // attempt" has to mean for admission: if the host now resolves somewhere the operator's
            // grant does not cover, the retry refuses instead of connecting there.
            let readmitted = match report.retries {
                0 => None,
                _ => Some(flux_system::net::guard_url_scoped_for_secret(
                    request.target.url().as_str(),
                    allow,
                )?),
            };
            let target = readmitted.as_ref().unwrap_or(&request.target);

            let budget = deadline.saturating_duration_since(tokio::time::Instant::now());
            let mut response = self
                .attempt(request, target, allow, budget, &mut admits)
                .await?;

            let retry_after = response
                .headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("retry-after"))
                .map(|(_, value)| value.as_str());
            let wait = retry::wait_after(&retry::Attempt {
                status: response.status,
                retry_after,
                retries: report.retries,
                waited: report.waited,
                // Measured *after* the attempt: what the wait has to fit inside is what is left.
                remaining: deadline.saturating_duration_since(tokio::time::Instant::now()),
                now: std::time::SystemTime::now(),
                jitter: retry::jitter(),
            });

            let Some(wait) = wait else {
                response.admits = admits;
                response.retries = report;
                return Ok(response);
            };

            // An `await`, never a blocking sleep. Cancellation in this codebase is the caller
            // dropping the operation future, so the wait has to be a suspension point: a cancelled
            // turn returns here instead of holding a thread until the far side's clock runs out.
            tokio::time::sleep(wait).await;
            report.waited = report.waited.saturating_add(wait);
            report.retries += 1;
        }
    }

    /// One attempt against `target`: authorize every carried secret against the destination that
    /// admission produced, send, and follow the bounded redirect chain.
    ///
    /// `target` rather than `request.target` is the whole reason this is a separate function. On the
    /// first attempt they are the same value; on a retry `target` is a freshly minted admission, and
    /// authorizing against *it* is what stops a retry inheriting a decision made for an earlier one.
    ///
    /// `budget` is what is left of the request's wall-clock allowance, not the request's own
    /// `timeout` — the chain, its redirects and the waits between attempts stay inside one deadline.
    async fn attempt(
        &self,
        request: &HttpRequest,
        target: &flux_system::secret_scope::GuardedSecretTarget,
        allow: &PrivateNetAllow,
        budget: Duration,
        admits: &mut Vec<PrivateAdmit>,
    ) -> Result<HttpResponse> {
        let operation = request.operation.as_str();
        let method = Method::from_bytes(request.method.as_bytes()).map_err(|_| {
            Error::Other(format!("{operation}: invalid method {:?}", request.method))
        })?;
        let mut headers = HeaderMap::new();
        for (name, value) in &request.headers {
            let name = HeaderName::from_bytes(name.as_bytes()).map_err(|e| {
                Error::Other(format!("{operation}: invalid header name `{name}`: {e}"))
            })?;
            // `expose` is the one door out of the carriage, and this is the place that has to use
            // it: a header value only becomes bytes where it becomes a request.
            let value = HeaderValue::from_str(value.expose()).map_err(|e| {
                Error::Other(format!(
                    "{operation}: invalid value for header `{name}`: {e}"
                ))
            })?;
            headers.insert(name, value);
        }

        // The **first** hop is authorized here too, not only the redirects (C-674).
        //
        // Before the family crossed a process boundary this was redundant: `http.request` refuses a
        // secret outside its grant before it ever reads the value, so a request that reached a
        // backend had already passed. It stops being redundant the moment the caller is on another
        // machine — the substrate that physically sends the credential must not take "the requester
        // says this was authorized" for the check. Locally it agrees by construction, because it is
        // the same allowlist measured against the same guard-vetted destination.
        Self::authorize_hop(
            request,
            target.url(),
            target.destination(),
            "the request to",
        )?;

        let response = egress::send_guarded(
            &self.http,
            egress::GuardedRequest {
                url: target.url().clone(),
                pinned: target.pinned().to_vec(),
                method,
                headers,
                body: request.body.clone(),
                timeout: budget,
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
                    "the redirect to",
                )?;
                Ok((url, pinned))
            },
            |url| {
                let Some(admit) = url
                    .host_str()
                    .and_then(|host| self.audit_admit(operation, host))
                else {
                    return;
                };
                // The sink above always hears about it; the *reported* list is bounded, because a
                // redirect chain and a retry budget are both bounded but a caller's allocation
                // should not depend on that.
                if admits.len() < flux_system::port::MAX_PRIVATE_ADMITS {
                    admits.push(admit);
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
            // Filled in by [`Self::send`], which is the only place that knows whether this attempt
            // was the last one and what the ones before it cost.
            admits: Vec::new(),
            retries: flux_system::port::RetryReport::default(),
        })
    }
}

impl GuardedHttp for NativeHttp {
    /// This backend exists to make HTTP requests, so it declares the family unconditionally. What a
    /// composition site does with that answer — whether a `System` carrying it announces the frame
    /// in a protocol handshake — is the composition site's decision, not this one's.
    fn serves_http(&self) -> bool {
        true
    }

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

    /// C-674, acceptance 4 — the native backend *reports* the private admission it made, and
    /// reports it as its own.
    ///
    /// The audit sink keeps working exactly as it did (C-652's behaviour, pinned by the
    /// selected-substrate tests below). What is new is that the same admission also rides the
    /// answer, because the process that admits is not always the process whose audit trail an
    /// operator reads. `substrate` is `None` here and that is the load-bearing part: it is how a
    /// caller tells an admission made in its own process from one made across a hop, which is what
    /// stops the event being recorded twice.
    #[tokio::test]
    async fn a_private_admit_is_reported_in_the_answer_as_this_process_s_own() {
        let base =
            one_shot("HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").await;
        let allow = PrivateNetAllow::Any;
        let native = NativeHttp::new(&WebOptions {
            grant_source: Some("cli:--allow-private-net".into()),
            ..Default::default()
        });

        let response = native
            .http_request(&request("web.fetch", &format!("{base}/ok"), &allow), &allow)
            .await
            .expect("an admitted loopback target is served");

        assert_eq!(
            response
                .admits
                .iter()
                .map(|admit| (
                    admit.host.as_str(),
                    admit.grant_source.as_str(),
                    admit.substrate.clone()
                ))
                .collect::<Vec<_>>(),
            vec![("127.0.0.1", "cli:--allow-private-net", None)],
            "loopback is a private range, so the admit is real: {:?}",
            response.admits
        );
    }

    /// C-674 — a public destination is not an admission, so it reports nothing.
    ///
    /// The report has to mean the same thing the audit event means, or a caller would read a
    /// routine request as a security event.
    #[tokio::test]
    async fn a_request_that_reaches_nothing_private_reports_no_admit() {
        let native = NativeHttp::new(&WebOptions::default());
        assert!(
            native.audit_admit("web.fetch", "example.com").is_none(),
            "a public host is not a private-network admission"
        );
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

/// C-701 — the rate-limit recovery this backend performs, driven end to end against a live server.
///
/// Every test here scripts a real loopback server and drives [`NativeHttp`] through the port, because
/// the properties under test are all joints: the wait comes from a header the far side chose, the
/// budget comes from the request, the guard and the secret scope re-run per attempt, and the
/// cancellation is the caller dropping the future. A unit test over the schedule alone (there is one,
/// in [`crate::retry`]) could not see any of them.
#[cfg(test)]
mod rate_limit_tests {
    use super::*;
    use flux_system::port::{HeaderValue as PortHeaderValue, HttpSecretScope};
    use flux_system::secret_scope::SecretAllowlist;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// One raw HTTP response, ready to be written to a socket.
    fn reply(status_line: &str, extra: &[&str], body: &str) -> String {
        let extra: String = extra.iter().map(|header| format!("{header}\r\n")).collect();
        format!(
            "HTTP/1.1 {status_line}\r\n{extra}content-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    /// A loopback server that answers a **scripted sequence** of responses — one per accepted
    /// connection — and remembers the raw bytes of every request it was sent.
    ///
    /// The script is what makes a retry observable: `[429, 200]` answers the rate limit first and
    /// succeeds second, so "the caller got a 200" *is* "the request was retried". Once the script
    /// runs out the last entry repeats, so a test that expected two attempts and got three fails on
    /// its count rather than hanging on an unanswered connection.
    struct Scripted {
        base: String,
        seen: Arc<Mutex<Vec<String>>>,
    }

    impl Scripted {
        fn attempts(&self) -> usize {
            self.seen.lock().unwrap().len()
        }

        fn requests(&self) -> Vec<String> {
            self.seen.lock().unwrap().clone()
        }
    }

    async fn scripted(script: Vec<String>) -> Scripted {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorded = seen.clone();
        tokio::spawn(async move {
            let mut turn = 0usize;
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut buf = vec![0u8; 8192];
                let read = socket.read(&mut buf).await.unwrap_or(0);
                recorded
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&buf[..read]).into_owned());
                let answer = script
                    .get(turn)
                    .or_else(|| script.last())
                    .cloned()
                    .unwrap_or_default();
                turn += 1;
                let _ = socket.write_all(answer.as_bytes()).await;
                let _ = socket.flush().await;
            }
        });
        Scripted {
            base: format!("http://{addr}"),
            seen,
        }
    }

    /// A GET through the guard, with an explicit wall-clock budget — the field the retry chain has
    /// to stay inside.
    fn get(url: &str, allow: &PrivateNetAllow, budget: Duration) -> HttpRequest {
        HttpRequest {
            operation: "http.request".to_string(),
            method: "GET".into(),
            target: flux_system::net::guard_url_scoped_for_secret(url, allow)
                .expect("the loopback fixture is admitted under its grant"),
            headers: Vec::new(),
            body: None,
            timeout: budget,
            max_response_bytes: 64 * 1024,
            secrets: HttpSecretScope::default(),
        }
    }

    /// An [`flux_plugin::EgressAudit`] that counts the private-destination admissions it is told
    /// about — the seam that shows whether the egress guard ran once or once per attempt.
    #[derive(Default)]
    struct CountingAudit(Mutex<Vec<String>>);

    impl flux_plugin::EgressAudit for CountingAudit {
        fn record_private_admit(&self, operation: &str, host: &str, _grant_source: &str) {
            self.0.lock().unwrap().push(format!("{operation} {host}"));
        }
    }

    /// C-701 acceptance 1 — `Retry-After` in its **delta-seconds** form is honored: the wait is at
    /// least what the server asked for, and the answer the caller gets is the retry's.
    ///
    /// A 429 is a definite answer — the far side received the request and declined to act on it —
    /// which is what makes waiting and asking again sound rather than a duplicate effect.
    #[tokio::test]
    async fn a_429_with_retry_after_delta_seconds_waits_that_long_and_retries() {
        let server = scripted(vec![
            reply("429 Too Many Requests", &["retry-after: 1"], "slow down"),
            reply("200 OK", &[], "served"),
        ])
        .await;
        let allow = PrivateNetAllow::Any;
        let native = NativeHttp::new(&WebOptions::default());

        let started = Instant::now();
        let response = native
            .http_request(
                &get(
                    &format!("{}/v1", server.base),
                    &allow,
                    Duration::from_secs(20),
                ),
                &allow,
            )
            .await
            .expect("a rate-limited request that succeeds on the retry is served");
        let elapsed = started.elapsed();

        assert_eq!(
            response.status, 200,
            "the caller gets the retry's answer, not the 429"
        );
        assert_eq!(String::from_utf8_lossy(&response.body), "served");
        assert_eq!(server.attempts(), 2, "exactly one retry was made");
        assert!(
            elapsed >= Duration::from_secs(1),
            "the server's delta-seconds must be waited out, not ignored: {elapsed:?}"
        );
        // C-701 acceptance 4 — the wait is reported, not silent.
        assert_eq!(response.retries.retries, 1);
        assert_eq!(response.retries.attempts(), 2);
        assert!(
            response.retries.waited >= Duration::from_secs(1),
            "the report accounts for the wait it took: {:?}",
            response.retries.waited
        );
        // The exact wording is pinned in `flux-system`'s own tests, where the wait is a fixed number
        // rather than one jitter widened; here the point is only that a surface has a sentence to
        // print instead of unexplained latency.
        let note = response
            .retries
            .note()
            .expect("a retried request has a note");
        assert!(
            note.starts_with("rate-limited, retried 1 time over 1."),
            "the note names the retries and the wait: {note}"
        );
    }

    /// C-701 acceptance 1 — `Retry-After` in its **HTTP-date** form is honored too.
    ///
    /// A date already in the past means "come back now", so the retry is immediate. That is what
    /// separates a parsed date from an unparsed one: an unusable header falls back to the
    /// exponential backoff, whose first step is half a second, so a sub-500ms wait can only mean the
    /// date was read.
    #[tokio::test]
    async fn a_429_with_retry_after_as_an_http_date_is_honored() {
        let server = scripted(vec![
            reply(
                "429 Too Many Requests",
                &["retry-after: Wed, 21 Oct 2015 07:28:00 GMT"],
                "slow down",
            ),
            reply("200 OK", &[], "served"),
        ])
        .await;
        let allow = PrivateNetAllow::Any;
        let native = NativeHttp::new(&WebOptions::default());

        let started = Instant::now();
        let response = native
            .http_request(
                &get(
                    &format!("{}/v1", server.base),
                    &allow,
                    Duration::from_secs(20),
                ),
                &allow,
            )
            .await
            .expect("a rate-limited request that succeeds on the retry is served");
        let elapsed = started.elapsed();

        assert_eq!(response.status, 200);
        assert_eq!(server.attempts(), 2, "exactly one retry was made");
        assert_eq!(response.retries.retries, 1);
        assert!(
            response.retries.waited < Duration::from_millis(500)
                && elapsed < Duration::from_secs(2),
            "an HTTP-date already past means retry now — a 500ms+ wait means the date was not \
             parsed and the backoff was used instead: waited {:?}, elapsed {elapsed:?}",
            response.retries.waited
        );
    }

    /// C-701 acceptance 1 — with **no** `Retry-After` the wait is a bounded exponential backoff with
    /// jitter: at least the first backoff step, and never more than that step plus the jitter span.
    #[tokio::test]
    async fn a_429_with_no_retry_after_backs_off_with_bounded_jitter() {
        let server = scripted(vec![
            reply("429 Too Many Requests", &[], "slow down"),
            reply("200 OK", &[], "served"),
        ])
        .await;
        let allow = PrivateNetAllow::Any;
        let native = NativeHttp::new(&WebOptions::default());

        let started = Instant::now();
        let response = native
            .http_request(
                &get(
                    &format!("{}/v1", server.base),
                    &allow,
                    Duration::from_secs(20),
                ),
                &allow,
            )
            .await
            .expect("a rate-limited request that succeeds on the retry is served");
        let elapsed = started.elapsed();

        assert_eq!(response.status, 200);
        assert_eq!(server.attempts(), 2, "exactly one retry was made");
        // The schedule is stated here rather than read off the implementation's constants: a test
        // that imports the number it is checking cannot catch that number being wrong.
        assert!(
            elapsed >= Duration::from_millis(500),
            "a headerless 429 still waits the first backoff step (500ms): {elapsed:?}"
        );
        let waited = response.retries.waited;
        assert!(
            waited >= Duration::from_millis(500)
                && waited <= Duration::from_millis(500) + Duration::from_millis(250),
            "the backoff is bounded — jitter widens it by at most 250ms, it does not unbound it: \
             {waited:?}"
        );
    }

    /// C-701 acceptance 2 — the request's wall-clock budget bounds the **whole chain including the
    /// waits**, and a retry that would overrun it returns the 429 instead of blocking past it.
    ///
    /// Both arms, in one test, because either alone proves nothing: with room in the budget the wait
    /// is taken and the retry answers, and with less budget than the wait needs the same 429 comes
    /// straight back. A backend that simply never retried would pass the second arm.
    #[tokio::test]
    async fn the_request_budget_bounds_the_wait_rather_than_being_overrun() {
        let allow = PrivateNetAllow::Any;
        let native = NativeHttp::new(&WebOptions::default());

        let roomy = scripted(vec![
            reply("429 Too Many Requests", &["retry-after: 1"], "slow down"),
            reply("200 OK", &[], "served"),
        ])
        .await;
        let served = native
            .http_request(
                &get(
                    &format!("{}/v1", roomy.base),
                    &allow,
                    Duration::from_secs(20),
                ),
                &allow,
            )
            .await
            .expect("a budget with room for the wait is served by the retry");
        assert_eq!(served.status, 200, "the wait fits, so the retry happens");
        assert_eq!(roomy.attempts(), 2);

        let tight = scripted(vec![
            reply("429 Too Many Requests", &["retry-after: 30"], "slow down"),
            reply("200 OK", &[], "served"),
        ])
        .await;
        let started = Instant::now();
        let refused = native
            .http_request(
                &get(
                    &format!("{}/v1", tight.base),
                    &allow,
                    Duration::from_secs(3),
                ),
                &allow,
            )
            .await
            .expect("a 429 is data, so an unretryable one is still a successful request");
        let elapsed = started.elapsed();

        assert_eq!(
            refused.status, 429,
            "a wait that does not fit the budget returns the 429"
        );
        assert_eq!(
            tight.attempts(),
            1,
            "and nothing is sent a second time: {:?}",
            tight.requests()
        );
        assert!(
            elapsed < Duration::from_secs(3),
            "the budget must not be blocked past — the answer comes back at once: {elapsed:?}"
        );
        assert_eq!(refused.retries, Default::default());
        assert!(
            refused.retries.note().is_none(),
            "nothing was retried, so there is nothing for a surface to explain"
        );
    }

    /// C-701 acceptance 2 — the wait is **cancellable**: a cancelled turn does not sit in a sleep.
    ///
    /// Cancellation in this codebase is the caller dropping the operation future, so that is what
    /// this does. The wait must therefore be an `await`, never a blocking sleep: dropping mid-wait
    /// has to return the thread at once *and* leave the retry unsent.
    #[tokio::test]
    async fn a_cancelled_turn_does_not_sit_in_the_retry_wait() {
        let server = scripted(vec![
            reply("429 Too Many Requests", &["retry-after: 20"], "slow down"),
            reply("200 OK", &[], "served"),
        ])
        .await;
        let allow = PrivateNetAllow::Any;
        let native = NativeHttp::new(&WebOptions::default());
        let request = get(
            &format!("{}/v1", server.base),
            &allow,
            Duration::from_secs(120),
        );

        let started = Instant::now();
        let outcome = tokio::time::timeout(
            Duration::from_millis(400),
            native.http_request(&request, &allow),
        )
        .await;
        let elapsed = started.elapsed();

        assert!(
            outcome.is_err(),
            "the request must still be waiting when the caller drops it — it returned instead"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "dropping the future must return immediately, not after the wait: {elapsed:?}"
        );
        // Give a stray retry every chance to reach the server before the count is read.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            server.attempts(),
            1,
            "a cancelled wait must never go on to send the retry: {:?}",
            server.requests()
        );
    }

    /// C-701 acceptance 3 — a retry re-runs the **per-hop secret re-authorization**, and does not
    /// reuse the decision the previous attempt made.
    ///
    /// The proof is a decision that could not have been made on the first attempt: the retry's
    /// response is a redirect to a host outside the carried secret's `to=` scope. Re-running the
    /// chain refuses it by name. A backend that authorized once before its retry loop — or that
    /// replayed the first attempt's admitted target — would follow that redirect and carry the
    /// credential to a host the operator never named.
    #[tokio::test]
    async fn a_retry_re_authorizes_the_secret_it_carries_on_every_hop_it_reaches() {
        let elsewhere = scripted(vec![reply("200 OK", &[], "should never be reached")]).await;
        let elsewhere_port = elsewhere
            .base
            .rsplit(':')
            .next()
            .expect("the fixture base URL ends in a port")
            .to_string();
        let server = scripted(vec![
            reply("429 Too Many Requests", &["retry-after: 0"], "slow down"),
            reply(
                "302 Found",
                &[&format!("location: http://localhost:{elsewhere_port}/next")],
                "",
            ),
        ])
        .await;
        let allow = PrivateNetAllow::Any;
        let native = NativeHttp::new(&WebOptions::default());

        let mut request = get(
            &format!("{}/v1", server.base),
            &allow,
            Duration::from_secs(20),
        );
        request.headers = vec![(
            "authorization".to_string(),
            PortHeaderValue::secret("FLUX_TEST_C701_TOKEN", "scoped-secret-42"),
        )];
        request.secrets = HttpSecretScope {
            allowlist: SecretAllowlist::parse(["FLUX_TEST_C701_TOKEN;to=127.0.0.1"]),
            carried: Vec::new(),
            principal: None,
        };

        let error = native
            .http_request(&request, &allow)
            .await
            .expect_err("the retry's redirect leaves the secret's scope and must be refused");
        let message = error.to_string();

        assert!(
            message.contains("refusing the redirect to localhost"),
            "the refusal is the per-hop scope check, re-run on the retry: {message}"
        );
        assert!(
            !message.contains("scoped-secret-42"),
            "a refusal never quotes the credential: {message}"
        );
        assert_eq!(
            server.attempts(),
            2,
            "the retry did happen — otherwise the refusal proves nothing: {:?}",
            server.requests()
        );
        assert_eq!(
            elsewhere.attempts(),
            0,
            "and nothing reached the out-of-scope host"
        );
    }

    /// C-701 acceptance 3 — and a retry re-runs the **egress guard**, rather than reusing the
    /// addresses the first attempt was vetted for.
    ///
    /// Loopback is a private range, so every admission is a real audit event. Two attempts must
    /// produce two admissions on the sink; a backend that re-sent to the previously vetted pins
    /// would produce one.
    #[tokio::test]
    async fn a_retry_re_admits_its_target_through_the_egress_guard() {
        let server = scripted(vec![
            reply("429 Too Many Requests", &["retry-after: 0"], "slow down"),
            reply("200 OK", &[], "served"),
        ])
        .await;
        let audit = Arc::new(CountingAudit::default());
        let allow = PrivateNetAllow::Any;
        let native = NativeHttp::new(&WebOptions {
            audit: Some(audit.clone()),
            grant_source: Some("test:grant".into()),
            ..Default::default()
        });

        let response = native
            .http_request(
                &get(
                    &format!("{}/v1", server.base),
                    &allow,
                    Duration::from_secs(20),
                ),
                &allow,
            )
            .await
            .expect("a rate-limited request that succeeds on the retry is served");

        assert_eq!(response.status, 200);
        assert_eq!(
            audit.0.lock().unwrap().as_slice(),
            [
                "web:http.request 127.0.0.1".to_string(),
                "web:http.request 127.0.0.1".to_string()
            ],
            "each attempt is admitted on its own, and each admission is audited"
        );
    }
}

/// C-675 — the **composition** the surface assembles: a selected native substrate serving the HTTP
/// port through this file's backend.
///
/// These tests live here rather than beside the two ops because the thing under test is the joint —
/// the confinement peer, the native `System` it composes, the backend the composition site attached
/// to that system, and the one egress client this file wraps — and because the sandboxed fixture
/// touches the process environment, which is safe to do exactly once, under one lock.
#[cfg(test)]
mod selected_substrate_tests {
    use super::*;
    use crate::fetch::WebFetchTool;
    use crate::http::HttpRequestTool;
    use flux_runtime::{Tool, ToolContext};
    use flux_system::sandbox::{Sandbox, SandboxMode, SandboxSettings};
    use flux_system::sandboxed::SandboxedSystem;
    use flux_system::{System, Workspace};
    use serde_json::json;
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Serialises the two tests that resolve a `Sandbox` from the process environment. The marker is
    /// read during `Sandbox::resolve` alone, so the lock only has to span the fixture.
    static SANDBOX_FIXTURE: Mutex<()> = Mutex::new(());

    /// An [`flux_plugin::EgressAudit`] that keeps what it was told, so a test can ask *which*
    /// backend made the request: the grant-source label rides along on every admit.
    #[derive(Default)]
    struct RecordingAudit(Mutex<Vec<String>>);

    impl RecordingAudit {
        fn admits(&self) -> Vec<String> {
            self.0.lock().unwrap().clone()
        }
    }

    impl flux_plugin::EgressAudit for RecordingAudit {
        fn record_private_admit(&self, operation: &str, _host: &str, grant_source: &str) {
            self.0
                .lock()
                .unwrap()
                .push(format!("{operation} via {grant_source}"));
        }
    }

    /// A one-shot loopback server that answers `body` with a 200 and reports its base URL.
    async fn one_shot_server(body: &'static str, content_type: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut buf = [0u8; 8192];
                let _ = socket.read(&mut buf).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.flush().await;
            }
        });
        format!("http://{addr}")
    }

    fn workspace(tag: &str) -> System {
        let dir = std::env::temp_dir().join(format!(
            "flux-web-c675-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        System::new(Workspace::new(&dir).unwrap())
    }
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    /// The **sandboxed selection**, assembled the way the surface assembles it: a native `System`
    /// carrying the HTTP backend the composition site built, admitted as the confinement peer.
    ///
    /// The nested-flux fixture is the deterministic door into `SandboxedSystem` — an ambient posture
    /// that already concluded (and disclosed) outer confinement — so nothing here depends on this
    /// machine having a bubblewrap.
    fn sandboxed_selection(tag: &str, http: Arc<dyn GuardedHttp>) -> SandboxedSystem {
        let _guard = SANDBOX_FIXTURE.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("FLUX_SANDBOXED", "1");
        let ambient = Sandbox::resolve(SandboxSettings {
            mode: SandboxMode::Require,
            ..SandboxSettings::off()
        });
        std::env::remove_var("FLUX_SANDBOXED");
        assert!(
            ambient.confined_by_parent(),
            "the fixture must really be confined by a parent, or it proves nothing"
        );
        SandboxedSystem::from_env(workspace(tag).with_sandbox(ambient).with_http(http))
            .expect("an ambient posture that already trusted the marker admits the peer")
    }

    /// C-675 (acceptance 1 + 2) — a sandboxed selection serves `http.request` through the one
    /// reviewed egress client, with the audit sink the *substrate's* backend was built with.
    ///
    /// Two sinks with different grant-source labels tell the two candidate paths apart: the op's own
    /// native backend (which a selection must never reach) and the substrate's. The loopback target
    /// is a private range, so an admit is recorded on whichever backend actually sent — which makes
    /// "the selection served it" and "the local client stayed off the path" one observation rather
    /// than two hopeful ones.
    #[tokio::test]
    async fn a_sandboxed_selection_serves_http_request_through_its_own_egress_client() {
        let base = one_shot_server("{\"ok\":true}", "application/json").await;
        let op_audit = Arc::new(RecordingAudit::default());
        let selection_audit = Arc::new(RecordingAudit::default());

        let tool = HttpRequestTool::new(&WebOptions {
            private_net: PrivateNetAllow::Any,
            audit: Some(op_audit.clone()),
            grant_source: Some("op-native".into()),
            ..Default::default()
        });
        let substrate_http: Arc<dyn GuardedHttp> = Arc::new(NativeHttp::new(&WebOptions {
            audit: Some(selection_audit.clone()),
            grant_source: Some("selection".into()),
            ..Default::default()
        }));
        let peer = sandboxed_selection("http-request", substrate_http);
        let ctx = ToolContext::new(Arc::new(workspace("http-request-ctx")))
            .with_execution_system(Arc::new(peer));

        let result = tool
            .execute(&ctx, json!({ "url": format!("{base}/v1") }))
            .await
            .expect("a sandboxed selection must serve http.request");

        let record: serde_json::Value = serde_json::from_str(&result.content)
            .expect("http.request answers with its record (C-304)");
        assert_eq!(
            record["status"], 200,
            "the selection's response must reach the op unchanged: {}",
            result.content
        );
        assert_eq!(record["body"]["ok"], true, "{}", result.content);
        assert_eq!(
            selection_audit.admits(),
            vec!["web:http.request via selection".to_string()],
            "the request must go through the substrate's own backend and audit sink"
        );
        assert!(
            op_audit.admits().is_empty(),
            "a selection in force must never fall back to the op's own local client: {:?}",
            op_audit.admits()
        );
    }

    /// C-675 (acceptance 1) — and the same for `web.fetch`, the other op the family moved.
    #[tokio::test]
    async fn a_sandboxed_selection_serves_web_fetch_through_its_own_egress_client() {
        let base = one_shot_server("<html><body><p>served</p></body></html>", "text/html").await;
        let op_audit = Arc::new(RecordingAudit::default());
        let selection_audit = Arc::new(RecordingAudit::default());

        let tool = WebFetchTool::new(&WebOptions {
            private_net: PrivateNetAllow::Any,
            audit: Some(op_audit.clone()),
            grant_source: Some("op-native".into()),
            ..Default::default()
        });
        let substrate_http: Arc<dyn GuardedHttp> = Arc::new(NativeHttp::new(&WebOptions {
            audit: Some(selection_audit.clone()),
            grant_source: Some("selection".into()),
            ..Default::default()
        }));
        let peer = sandboxed_selection("web-fetch", substrate_http);
        let ctx = ToolContext::new(Arc::new(workspace("web-fetch-ctx")))
            .with_execution_system(Arc::new(peer));

        let result = tool
            .execute(&ctx, json!({ "url": base }))
            .await
            .expect("a sandboxed selection must serve web.fetch");

        assert!(
            result.content.starts_with("[200 OK]"),
            "the selection's read must reach the op unchanged: {}",
            result.content
        );
        assert!(result.content.contains("served"), "{}", result.content);
        assert_eq!(
            selection_audit.admits(),
            vec!["web:web.fetch via selection".to_string()],
            "the read must go through the substrate's own backend and audit sink"
        );
        assert!(
            op_audit.admits().is_empty(),
            "a selection in force must never fall back to the op's own local client: {:?}",
            op_audit.admits()
        );
    }

    /// C-675 (acceptance 2) — the branch stays **kind-blind**, so a selection that serves no HTTP
    /// still refuses rather than borrowing the caller's process.
    ///
    /// The composition is what serves HTTP, not the substrate's kind: the same confinement peer,
    /// composed over a system with no backend attached, answers the port's `Unserved` — and the
    /// live loopback fixture is never contacted, which is what makes the refusal mean "nothing sent"
    /// rather than "nothing reachable".
    #[tokio::test]
    async fn a_selection_with_no_http_backend_refuses_instead_of_sending_locally() {
        let base = one_shot_server("{\"ok\":true}", "application/json").await;
        let op_audit = Arc::new(RecordingAudit::default());
        let tool = HttpRequestTool::new(&WebOptions {
            private_net: PrivateNetAllow::Any,
            audit: Some(op_audit.clone()),
            grant_source: Some("op-native".into()),
            ..Default::default()
        });
        let bare = sandboxed_selection_without_http("no-backend");
        let ctx = ToolContext::new(Arc::new(workspace("no-backend-ctx")))
            .with_execution_system(Arc::new(bare));

        let error = tool
            .execute(&ctx, json!({ "url": format!("{base}/v1") }))
            .await
            .expect_err("a substrate with no HTTP backend must refuse the request");

        assert!(
            error.to_string().starts_with(flux_system::port::UNSERVED),
            "the refusal must be the port's own, not an improvised error: {error}"
        );
        assert!(
            op_audit.admits().is_empty(),
            "nothing may be sent from the caller's process while a selection is in force: {:?}",
            op_audit.admits()
        );
    }

    /// The same peer, composed over a system the surface attached nothing to.
    fn sandboxed_selection_without_http(tag: &str) -> SandboxedSystem {
        let _guard = SANDBOX_FIXTURE.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("FLUX_SANDBOXED", "1");
        let ambient = Sandbox::resolve(SandboxSettings {
            mode: SandboxMode::Require,
            ..SandboxSettings::off()
        });
        std::env::remove_var("FLUX_SANDBOXED");
        SandboxedSystem::from_env(workspace(tag).with_sandbox(ambient))
            .expect("an ambient posture that already trusted the marker admits the peer")
    }
}
