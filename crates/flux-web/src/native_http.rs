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

use flux_core::{Error, Result};
use flux_system::net::PrivateNetAllow;
use flux_system::port::{Guarded, GuardedHttp, HttpRequest, HttpResponse, PrivateAdmit};
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
            request.target.url(),
            request.target.destination(),
            "the request to",
        )?;

        // Collected in hop order alongside the audit sink, so the answer can carry what happened
        // rather than leaving a caller across a link to reconstruct it from a list of hosts.
        let mut admits: Vec<PrivateAdmit> = Vec::new();
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
                // redirect chain is bounded but a caller's allocation should not depend on that.
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
            admits,
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
