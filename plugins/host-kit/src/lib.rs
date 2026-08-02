//! `host-kit` — the shared SDK for flux integration plugins (story D-08).
//!
//! It wraps flux-plugin's guest protocol so a plugin is mostly "declare ops + implement each against a
//! vendor API": a typed [`Host`] for the host-capability callbacks (secret-by-purpose, HTTP with
//! auth-by-scheme injection, reference-based IO, datasource-record contribution) and a [`PluginBuilder`] that collects
//! a manifest + op handlers and serves them. Plugins never read state files or hold raw tokens for the
//! auth-injection path — the host resolves secrets and injects them. Endpoints are addressed **by
//! reference** (D-32): the host resolves a declared endpoint's URL and performs the IO; there is no
//! capability that hands a URL string back to the plugin.
//!
//! ```ignore
//! use host_kit::*;
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(Deserialize, Serialize, schemars::JsonSchema)]
//! #[serde(deny_unknown_fields)]
//! struct PingInput {}
//!
//! #[derive(Serialize, schemars::JsonSchema)]
//! struct PingOutput {
//!     status: serde_json::Value,
//! }
//!
//! fn main() -> Result<(), String> {
//!     PluginBuilder::new("acme", "0.1.0")
//!         .capabilities(Caps { http: true, secrets: vec!["ACME_TOKEN".into()], ..Caps::default() })
//!         .auth(AuthMethod { purpose: "api_token".into(), env: vec!["ACME_TOKEN".into()], ..Default::default() })
//!         .endpoint(EndpointSpec { name: "acme.endpoint".into(), env: vec!["ACME_URL".into()], ..Default::default() })
//!         .operation_typed::<PingInput, PingOutput>(
//!             read_op_typed::<PingInput>("acme.ping", "Ping the API"),
//!             |_in, host| {
//!                 let status = host.get_json_ref("acme.endpoint", "/ping", Some("api_token"))?;
//!                 Ok(PingOutput { status })
//!             },
//!         )
//!         .try_serve()
//! }
//! ```

use std::collections::HashMap;

use base64::Engine as _;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};

pub mod preflight;
pub use preflight::schema_preflight;

// Re-export the protocol vocabulary so a plugin depends only on host-kit.
pub use flux_datasource::{Declaration, EntitySchema, Link, Record, SchemaField, Source};
pub use flux_plugin_protocol::{
    AuthMethod, AuthScheme, ConfigSpec, EndpointSpec, GuestHost, OAuth2Spec, OAuthGrant,
    OAuthRedirect, OperationSpec, PlatformSourcing, PluginCapabilities as Caps, PluginHandler,
    PluginManifest, SignalMatch, ToolGroup, VendorReach, KIND_TURN_INTENT, VALIDATE_OP,
};
pub use flux_spec::{Effect, Idempotency, Risk, StagingDisposition};

/// Re-export `schemars` so a plugin crate can `#[derive(host_kit::schemars::JsonSchema)]`
/// (or `Deserialize`) on its op-input structs without adding its own `schemars` dependency —
/// host-kit is the single owner of the plugin-side schema-derivation path (story D-36).
pub use schemars;

/// Derive the provider-facing JSON Schema for a typed plugin op input.
///
/// This is the plugin-side counterpart of `flux_spec::tool_input_schema::<T>()` (same
/// semantics: strips the root `$schema`/`title`/`description` while preserving field
/// descriptions and definitions). Every plugin `OperationSpec` should get its `input_schema`
/// from here via a `#[derive(Deserialize, schemars::JsonSchema)]` struct, so the schema the
/// model sees and the fields the handler reads cannot drift (D-36).
///
/// Prefer the [`read_op_typed`] / [`write_op_typed`] helpers, which call this for you.
pub fn op_input_schema<T: schemars::JsonSchema + 'static>() -> Value {
    flux_spec::tool_input_schema::<T>()
}

/// Derive the successful-result JSON Schema for a typed plugin operation.
pub fn op_output_schema<T: schemars::JsonSchema + 'static>() -> Value {
    flux_spec::tool_output_schema::<T>()
}

/// A typed view over the host-capability channel, handed to each op handler.
pub struct Host<'a> {
    inner: &'a mut dyn GuestHost,
}

impl<'a> Host<'a> {
    /// Wrap a guest-host implementation. Mostly used by plugin unit tests around [`MockHost`].
    pub fn new(inner: &'a mut dyn GuestHost) -> Self {
        Self { inner }
    }
}

/// A host HTTP response.
pub struct HttpResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response body (truncated by the host to a sane cap).
    pub body: String,
}

/// The result of a host `process.run`.
pub struct ProcessOutput {
    /// Captured standard output.
    pub stdout: String,
    /// Captured standard error.
    pub stderr: String,
    /// The process exit code (`-1` if unknown).
    pub exit_code: i64,
}

/// A drained snapshot of a host-managed background process (from [`Host::process_read`]): the output
/// accumulated since the previous read plus the current liveness.
pub struct ProcRead {
    /// stdout accumulated since the last read (drained).
    pub stdout: String,
    /// stderr accumulated since the last read (drained).
    pub stderr: String,
    /// Whether the process is still running.
    pub running: bool,
    /// The exit code once it has exited (`None` while running).
    pub exit_code: Option<i64>,
}

/// Liveness of a host-managed background process (from [`Host::process_status`]).
pub struct ProcStatus {
    /// Whether the process is still running.
    pub running: bool,
    /// The exit code once it has exited (`None` while running).
    pub exit_code: Option<i64>,
}

/// A binary HTTP response (from [`Host::http_bytes`]): the raw response bytes, never text-truncated.
pub struct HttpBytesResponse {
    /// HTTP status code.
    pub status: u16,
    /// The raw response body bytes.
    pub bytes: Vec<u8>,
}

impl HttpResponse {
    /// Parse the body as JSON.
    pub fn json(&self) -> Result<Value, String> {
        serde_json::from_str(&self.body).map_err(|e| format!("response not JSON: {e}"))
    }
    /// Whether the status is 2xx.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

impl Host<'_> {
    /// Check whether a declared auth purpose can be resolved without returning its secret value to
    /// the plugin. Use this to select an optional authenticated backend before an `http.do` call;
    /// the eventual request should still pass `auth_purpose` for host-side injection.
    pub fn auth_available(&mut self, purpose: &str) -> Result<bool, String> {
        let v = self
            .inner
            .host_call("auth.available", json!({ "purpose": purpose }))?;
        v.get("available")
            .and_then(Value::as_bool)
            .ok_or_else(|| "auth.available: host returned no boolean".into())
    }

    /// Resolve a secret by purpose (an auth-method name declared in the manifest).
    pub fn secret(&mut self, purpose: &str) -> Result<String, String> {
        let v = self
            .inner
            .host_call("secret", json!({ "purpose": purpose }))?;
        v.get("value")
            .and_then(|x| x.as_str())
            .map(String::from)
            .ok_or_else(|| "secret: host returned no value".into())
    }

    /// Read a declared **non-secret** config value by name via the gated `config` host capability
    /// (D-32) — e.g. jira's Atlassian `cloud_id`. Deny-by-default: the host refuses undeclared
    /// names, and refuses any declared env key that is secret-classified, so this can never return
    /// a secret value. This replaces the config reads that abused the retired `endpoint`
    /// URL-handback; URLs themselves stay host-side (address endpoints by reference instead).
    pub fn config(&mut self, name: &str) -> Result<String, String> {
        let v = self.inner.host_call("config", json!({ "name": name }))?;
        v.get("value")
            .and_then(|x| x.as_str())
            .map(String::from)
            .ok_or_else(|| "config: host returned no value".into())
    }

    /// Make an HTTP request through the host. `auth_purpose` (when set) names an auth method the host
    /// resolves and injects per its declared [`AuthScheme`] (Bearer/Basic/Header/Query) — the plugin
    /// never sees the raw token.
    pub fn http(
        &mut self,
        method: &str,
        url: &str,
        auth_purpose: Option<&str>,
        headers: &[(&str, &str)],
        body: Option<&str>,
    ) -> Result<HttpResponse, String> {
        let mut payload = json!({ "method": method, "url": url });
        if let Some(p) = auth_purpose {
            payload["auth_purpose"] = json!(p);
        }
        if !headers.is_empty() {
            let map: serde_json::Map<String, Value> = headers
                .iter()
                .map(|(k, v)| ((*k).to_string(), json!(v)))
                .collect();
            payload["headers"] = Value::Object(map);
        }
        if let Some(b) = body {
            payload["body"] = json!(b);
        }
        let v = self.inner.host_call("http.do", payload)?;
        Ok(HttpResponse {
            status: v.get("status").and_then(|x| x.as_u64()).unwrap_or(0) as u16,
            body: v
                .get("body")
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string(),
        })
    }

    /// Convenience: GET a URL (optional auth purpose) and parse the JSON body, erroring on non-2xx.
    pub fn get_json(&mut self, url: &str, auth_purpose: Option<&str>) -> Result<Value, String> {
        let resp = self.http("GET", url, auth_purpose, &[], None)?;
        if !resp.is_success() {
            return Err(format!("GET {url} → {} {}", resp.status, resp.body));
        }
        resp.json()
    }

    /// Convenience: send a JSON body with `method` (optional auth purpose) and parse the response.
    pub fn send_json(
        &mut self,
        method: &str,
        url: &str,
        auth_purpose: Option<&str>,
        body: &Value,
    ) -> Result<Value, String> {
        let s = serde_json::to_string(body).map_err(|e| e.to_string())?;
        let resp = self.http(
            method,
            url,
            auth_purpose,
            &[("content-type", "application/json")],
            Some(&s),
        )?;
        if !resp.is_success() {
            return Err(format!("{method} {url} → {} {}", resp.status, resp.body));
        }
        resp.json()
    }

    /// Make an HTTP request through the host **by endpoint reference** — the plugin never holds a
    /// URL. The host resolves `endpoint_ref` (a named manifest endpoint, or a discovered
    /// `@endpoint/<id>`), joins `path` onto the resolved base, and injects any credential the
    /// reference carries host-side. `auth_purpose` (when set) names a manifest auth method the host
    /// injects per its declared scheme; `headers` are extra request headers (e.g. a runtime session
    /// token) — same as [`http`](Self::http), but the URL stays host-only.
    pub fn http_ref(
        &mut self,
        endpoint_ref: &str,
        method: &str,
        path: &str,
        auth_purpose: Option<&str>,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
    ) -> Result<HttpResponse, String> {
        let mut payload = json!({ "method": method, "endpoint_ref": endpoint_ref, "path": path });
        if let Some(p) = auth_purpose {
            payload["auth_purpose"] = json!(p);
        }
        if !headers.is_empty() {
            let map: serde_json::Map<String, Value> = headers
                .iter()
                .map(|(k, v)| ((*k).to_string(), json!(v)))
                .collect();
            payload["headers"] = Value::Object(map);
        }
        if let Some(b) = body {
            payload["body_b64"] = json!(base64::engine::general_purpose::STANDARD.encode(b));
        }
        let v = self.inner.host_call("http.do", payload)?;
        Ok(HttpResponse {
            status: v.get("status").and_then(|x| x.as_u64()).unwrap_or(0) as u16,
            body: v
                .get("body")
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string(),
        })
    }

    /// Make an HTTP request with a **byte-exact** body and/or response **by endpoint reference** —
    /// the ref-based mirror of [`http_bytes`](Self::http_bytes) (D-32): binary upload/download
    /// (attachment fetches, multipart uploads) without the plugin ever holding a URL. The host
    /// resolves `endpoint_ref`, joins `path`, and injects `auth_purpose` per its declared scheme;
    /// `body` (when set) is sent verbatim; `binary_response` asks for the raw response bytes
    /// (otherwise the response body's bytes are its UTF-8 text).
    #[allow(clippy::too_many_arguments)]
    pub fn http_bytes_ref(
        &mut self,
        endpoint_ref: &str,
        method: &str,
        path: &str,
        auth_purpose: Option<&str>,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
        binary_response: bool,
    ) -> Result<HttpBytesResponse, String> {
        let mut payload = json!({ "method": method, "endpoint_ref": endpoint_ref, "path": path });
        if let Some(p) = auth_purpose {
            payload["auth_purpose"] = json!(p);
        }
        if !headers.is_empty() {
            let map: serde_json::Map<String, Value> = headers
                .iter()
                .map(|(k, v)| ((*k).to_string(), json!(v)))
                .collect();
            payload["headers"] = Value::Object(map);
        }
        if let Some(b) = body {
            payload["body_b64"] = json!(base64::engine::general_purpose::STANDARD.encode(b));
        }
        if binary_response {
            payload["response_binary"] = json!(true);
        }
        let v = self.inner.host_call("http.do", payload)?;
        let status = v.get("status").and_then(|x| x.as_u64()).unwrap_or(0) as u16;
        let bytes = if let Some(b64) = v.get("body_b64").and_then(|x| x.as_str()) {
            base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| format!("http_bytes_ref: bad body_b64: {e}"))?
        } else if let Some(s) = v.get("body").and_then(|x| x.as_str()) {
            s.as_bytes().to_vec()
        } else {
            Vec::new()
        };
        Ok(HttpBytesResponse { status, bytes })
    }

    /// Convenience: GET an endpoint-reference path (optional auth purpose) and parse the JSON body,
    /// erroring on non-2xx. The ref-based mirror of [`get_json`](Self::get_json).
    pub fn get_json_ref(
        &mut self,
        endpoint_ref: &str,
        path: &str,
        auth_purpose: Option<&str>,
    ) -> Result<Value, String> {
        let resp = self.http_ref(endpoint_ref, "GET", path, auth_purpose, &[], None)?;
        if !resp.is_success() {
            return Err(format!(
                "GET {endpoint_ref} {path} → {} {}",
                resp.status, resp.body
            ));
        }
        resp.json()
    }

    /// Convenience: send a JSON body to an endpoint-reference path with `method` (optional auth
    /// purpose) and parse the response. The ref-based mirror of [`send_json`](Self::send_json).
    pub fn send_json_ref(
        &mut self,
        endpoint_ref: &str,
        method: &str,
        path: &str,
        auth_purpose: Option<&str>,
        body: &Value,
    ) -> Result<Value, String> {
        let s = serde_json::to_string(body).map_err(|e| e.to_string())?;
        let resp = self.http_ref(
            endpoint_ref,
            method,
            path,
            auth_purpose,
            &[("content-type", "application/json")],
            Some(s.as_bytes()),
        )?;
        if !resp.is_success() {
            return Err(format!(
                "{method} {endpoint_ref} {path} → {} {}",
                resp.status, resp.body
            ));
        }
        resp.json()
    }

    /// Run an allow-listed subprocess through the host (e.g. `kubectl`). `argv[0]` must be in the
    /// plugin's granted `process` capabilities. Returns stdout/stderr/exit code.
    pub fn run(&mut self, argv: &[&str], timeout_secs: u64) -> Result<ProcessOutput, String> {
        let v = self.inner.host_call(
            "process.run",
            json!({ "argv": argv, "timeout_secs": timeout_secs }),
        )?;
        Ok(ProcessOutput {
            stdout: v
                .get("stdout")
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string(),
            stderr: v
                .get("stderr")
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string(),
            exit_code: v.get("exit_code").and_then(|x| x.as_i64()).unwrap_or(-1),
        })
    }

    /// Spawn an allow-listed **long-lived background** subprocess through the host (e.g.
    /// `kubectl port-forward`). `argv[0]` must be in the plugin's granted `process` capabilities; the
    /// optional `env` overrides are applied on top of the host's cleared+allow-listed environment.
    /// Returns an opaque `proc_id` for [`process_read`](Self::process_read) /
    /// [`process_status`](Self::process_status) / [`process_kill`](Self::process_kill) — the proc
    /// persists across op calls, so start it in one call and stop it in a later one.
    pub fn process_spawn(&mut self, argv: &[&str], env: &[(&str, &str)]) -> Result<u64, String> {
        let mut payload = json!({ "argv": argv });
        if !env.is_empty() {
            let map: serde_json::Map<String, Value> = env
                .iter()
                .map(|(k, v)| ((*k).to_string(), json!(v)))
                .collect();
            payload["env"] = Value::Object(map);
        }
        let v = self.inner.host_call("process.spawn", payload)?;
        v.get("proc_id")
            .and_then(|x| x.as_u64())
            .ok_or_else(|| "process.spawn: host returned no proc_id".into())
    }

    /// Drain the output a background process has produced since the last read, plus its liveness.
    pub fn process_read(&mut self, proc_id: u64) -> Result<ProcRead, String> {
        let v = self
            .inner
            .host_call("process.read", json!({ "proc_id": proc_id }))?;
        Ok(ProcRead {
            stdout: v
                .get("stdout")
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string(),
            stderr: v
                .get("stderr")
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string(),
            running: v.get("running").and_then(|x| x.as_bool()).unwrap_or(false),
            exit_code: v.get("exit_code").and_then(|x| x.as_i64()),
        })
    }

    /// Poll a background process's liveness (non-blocking) without draining its output.
    pub fn process_status(&mut self, proc_id: u64) -> Result<ProcStatus, String> {
        let v = self
            .inner
            .host_call("process.status", json!({ "proc_id": proc_id }))?;
        Ok(ProcStatus {
            running: v.get("running").and_then(|x| x.as_bool()).unwrap_or(false),
            exit_code: v.get("exit_code").and_then(|x| x.as_i64()),
        })
    }

    /// Kill a background process and drop it from the host registry.
    pub fn process_kill(&mut self, proc_id: u64) -> Result<(), String> {
        self.inner
            .host_call("process.kill", json!({ "proc_id": proc_id }))?;
        Ok(())
    }

    /// Make an HTTP request with a **byte-exact** body and/or response — for binary upload/download
    /// (file uploads, attachment fetches) where the text [`http`](Self::http) path would corrupt
    /// non-UTF-8 bytes. `body` (when set) is sent verbatim; `binary_response` asks the host to return
    /// the raw response bytes (otherwise the response body's bytes are its UTF-8 text). `auth_purpose`
    /// is injected by the host exactly as for [`http`](Self::http) — the plugin never sees the token.
    pub fn http_bytes(
        &mut self,
        method: &str,
        url: &str,
        auth_purpose: Option<&str>,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
        binary_response: bool,
    ) -> Result<HttpBytesResponse, String> {
        let mut payload = json!({ "method": method, "url": url });
        if let Some(p) = auth_purpose {
            payload["auth_purpose"] = json!(p);
        }
        if !headers.is_empty() {
            let map: serde_json::Map<String, Value> = headers
                .iter()
                .map(|(k, v)| ((*k).to_string(), json!(v)))
                .collect();
            payload["headers"] = Value::Object(map);
        }
        if let Some(b) = body {
            payload["body_b64"] = json!(base64::engine::general_purpose::STANDARD.encode(b));
        }
        if binary_response {
            payload["response_binary"] = json!(true);
        }
        let v = self.inner.host_call("http.do", payload)?;
        let status = v.get("status").and_then(|x| x.as_u64()).unwrap_or(0) as u16;
        let bytes = if let Some(b64) = v.get("body_b64").and_then(|x| x.as_str()) {
            base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| format!("http_bytes: bad body_b64: {e}"))?
        } else if let Some(s) = v.get("body").and_then(|x| x.as_str()) {
            s.as_bytes().to_vec()
        } else {
            Vec::new()
        };
        Ok(HttpBytesResponse { status, bytes })
    }

    /// Contribute records to the host's datasource index (they become searchable knowledge).
    pub fn contribute(&mut self, records: &[Record]) -> Result<usize, String> {
        let v = self
            .inner
            .host_call("datasource.records", json!({ "records": records }))?;
        Ok(v.get("indexed").and_then(|x| x.as_u64()).unwrap_or(0) as usize)
    }

    /// Open a raw socket connection through the host (gated by the plugin's `conn` capability; TCP is
    /// SSRF-guarded). Returns an opaque id for [`conn_write`](Self::conn_write) /
    /// [`conn_read`](Self::conn_read) / [`conn_close`](Self::conn_close) — the way a plugin drives a
    /// wire protocol (SQL or the Docker socket) the host never speaks itself.
    pub fn conn_dial(&mut self, target: ConnTarget) -> Result<u64, String> {
        let payload = match target {
            ConnTarget::Tcp { host, port } => json!({ "kind": "tcp", "host": host, "port": port }),
            ConnTarget::Unix { path } => json!({ "kind": "unix", "path": path }),
        };
        let v = self.inner.host_call("conn.dial", payload)?;
        v.get("conn_id")
            .and_then(|x| x.as_u64())
            .ok_or_else(|| "conn.dial: host returned no conn_id".into())
    }

    /// Open a raw socket connection **by endpoint reference** — the plugin passes the ref, never the
    /// host:port. The host resolves `endpoint_ref` (named manifest endpoint or discovered
    /// `@endpoint/<id>`) to a host:port and dials it under the same SSRF/grant guard as
    /// [`conn_dial`](Self::conn_dial). Returns the opaque connection id. This is how a raw-socket
    /// plugin (for example SQL) reaches a discovered endpoint without ever holding a URL.
    pub fn conn_dial_ref(&mut self, endpoint_ref: &str) -> Result<u64, String> {
        let v = self
            .inner
            .host_call("conn.dial", json!({ "endpoint_ref": endpoint_ref }))?;
        v.get("conn_id")
            .and_then(|x| x.as_u64())
            .ok_or_else(|| "conn.dial: host returned no conn_id".into())
    }

    /// Materialize a credential **reference** into its raw secret value via the gated `credential`
    /// host capability — for raw-socket in-band-auth protocols (e.g. Postgres SCRAM) that must speak
    /// the handshake themselves. Deny-by-default: the plugin's manifest must grant `credential`. The
    /// value is delivered only to the trusted plugin binary and registered with the host redactor, so
    /// it never leaks into model-visible output. `credential_ref` is a `scheme/...` string (e.g.
    /// `kubernetes/monitoring/pg-creds/password`).
    pub fn credential(&mut self, credential_ref: &str) -> Result<String, String> {
        let v = self
            .inner
            .host_call("credential", json!({ "credential_ref": credential_ref }))?;
        v.get("value")
            .and_then(|x| x.as_str())
            .map(String::from)
            .ok_or_else(|| "credential: host returned no value".into())
    }

    /// Materialize the credential **attached to an endpoint reference** via the gated `credential`
    /// host capability — the host looks the endpoint's `credential_ref` up in its registry and
    /// resolves it (cross-plugin grants/audit apply). Same deny-by-default + redaction guarantees as
    /// [`credential`](Self::credential); the plugin passes only the `endpoint_ref`.
    pub fn credential_for_endpoint(&mut self, endpoint_ref: &str) -> Result<String, String> {
        let v = self
            .inner
            .host_call("credential", json!({ "endpoint_ref": endpoint_ref }))?;
        v.get("value")
            .and_then(|x| x.as_str())
            .map(String::from)
            .ok_or_else(|| "credential: host returned no value".into())
    }

    /// **Host-terminate** the in-band-auth handshake of a raw-socket protocol on an already-dialed
    /// connection (D-31). The host speaks the protocol's startup + authentication (e.g. PostgreSQL
    /// StartupMessage + SCRAM-SHA-256/MD5) using a credential it resolves **host-side**, and hands
    /// back a POST-AUTH connection: the plugin keeps driving the same `conn_id` (Simple Query, etc.)
    /// but **never receives the password**. This is the stricter successor to [`credential`](Self::credential)
    /// for host-terminated protocols — the plugin holds no secret value at all.
    ///
    /// `credential` names WHERE the host finds the secret (a declared auth method, an explicit
    /// credential reference, or a discovered endpoint's attached credential) — never the value.
    /// Returns the negotiated non-secret connection parameters (notably `server_version`).
    #[allow(clippy::too_many_arguments)]
    pub fn conn_authenticate(
        &mut self,
        conn_id: u64,
        protocol: &str,
        user: &str,
        database: &str,
        application_name: Option<&str>,
        credential: PgCredential,
        timeout_ms: Option<u64>,
    ) -> Result<HandshakeInfo, String> {
        let mut payload = json!({
            "conn_id": conn_id,
            "protocol": protocol,
            "user": user,
            "database": database,
        });
        if let Some(app) = application_name {
            payload["application_name"] = json!(app);
        }
        if let Some(ms) = timeout_ms {
            payload["timeout_ms"] = json!(ms);
        }
        match credential {
            PgCredential::AuthPurpose(p) => payload["auth_purpose"] = json!(p),
            PgCredential::CredentialRef(r) => payload["credential_ref"] = json!(r),
            PgCredential::EndpointRef(r) => payload["endpoint_ref"] = json!(r),
        }
        let v = self.inner.host_call("conn.authenticate", payload)?;
        let parameters = v
            .get("parameters")
            .and_then(|p| p.as_object())
            .map(|o| {
                o.iter()
                    .filter_map(|(k, val)| val.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        Ok(HandshakeInfo {
            server_version: v
                .get("server_version")
                .and_then(|x| x.as_str())
                .map(String::from),
            parameters,
        })
    }

    /// Write bytes to an open connection; returns the number written.
    pub fn conn_write(&mut self, conn_id: u64, data: &[u8]) -> Result<usize, String> {
        let b64 = base64::engine::general_purpose::STANDARD.encode(data);
        let v = self
            .inner
            .host_call("conn.write", json!({ "conn_id": conn_id, "data_b64": b64 }))?;
        Ok(v.get("written").and_then(|x| x.as_u64()).unwrap_or(0) as usize)
    }

    /// Read up to `max` bytes from an open connection; an empty `Vec` means EOF.
    pub fn conn_read(&mut self, conn_id: u64, max: usize) -> Result<Vec<u8>, String> {
        self.conn_read_timed(conn_id, max, None)
    }

    /// Read up to `max` bytes from an open connection with an optional per-call deadline
    /// (`timeout_ms`, milliseconds). On timeout the host returns an empty `Vec` plus the connection
    /// left open — `ConnStream` surfaces this as an [`std::io::ErrorKind::TimedOut`] so a plugin's
    /// wire-protocol loop can distinguish a deadline from a clean EOF (D-45).
    pub fn conn_read_timed(
        &mut self,
        conn_id: u64,
        max: usize,
        timeout_ms: Option<u64>,
    ) -> Result<Vec<u8>, String> {
        let mut req = json!({ "conn_id": conn_id, "max": max });
        if let Some(ms) = timeout_ms {
            req["timeout_ms"] = json!(ms);
        }
        let v = self.inner.host_call("conn.read", req)?;
        let timed_out = v
            .get("timed_out")
            .and_then(|x| x.as_bool())
            .unwrap_or(false);
        if timed_out {
            return Err(format!(
                "conn.read: timed out after {}ms",
                timeout_ms.unwrap_or(0)
            ));
        }
        let b64 = v.get("data_b64").and_then(|x| x.as_str()).unwrap_or("");
        base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| format!("conn.read: bad base64: {e}"))
    }

    /// Close an open connection.
    pub fn conn_close(&mut self, conn_id: u64) -> Result<(), String> {
        self.inner
            .host_call("conn.close", json!({ "conn_id": conn_id }))?;
        Ok(())
    }

    /// Store bytes in the host's content-addressed scratch store (gated by the `blob` capability);
    /// returns an opaque `blob_ref` to pass as a `blob_ref` input instead of inlining base64.
    pub fn blob_put(&mut self, name: &str, data: &[u8]) -> Result<String, String> {
        let b64 = base64::engine::general_purpose::STANDARD.encode(data);
        let v = self
            .inner
            .host_call("blob.put", json!({ "name": name, "data_b64": b64 }))?;
        v.get("blob_ref")
            .and_then(|x| x.as_str())
            .map(String::from)
            .ok_or_else(|| "blob.put: host returned no blob_ref".into())
    }

    /// Fetch the bytes behind a `blob_ref`.
    pub fn blob_get(&mut self, blob_ref: &str) -> Result<Vec<u8>, String> {
        let v = self
            .inner
            .host_call("blob.get", json!({ "blob_ref": blob_ref }))?;
        let b64 = v.get("data_b64").and_then(|x| x.as_str()).unwrap_or("");
        base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| format!("blob.get: bad base64: {e}"))
    }

    /// Metadata for a `blob_ref` (name, size, sha256).
    pub fn blob_info(&mut self, blob_ref: &str) -> Result<BlobInfo, String> {
        let v = self
            .inner
            .host_call("blob.info", json!({ "blob_ref": blob_ref }))?;
        Ok(BlobInfo {
            name: v
                .get("name")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            size: v.get("size").and_then(|x| x.as_u64()).unwrap_or(0) as usize,
            sha256: v
                .get("sha256")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
        })
    }
}

/// A socket target for [`Host::conn_dial`].
pub enum ConnTarget<'a> {
    /// A TCP `host:port`.
    Tcp { host: &'a str, port: u16 },
    /// A local Unix-domain socket path.
    Unix { path: &'a str },
}

/// Where the host finds the credential for a host-terminated handshake ([`Host::conn_authenticate`],
/// D-31). Every variant is a *location*, never a value — the plugin never holds the secret.
pub enum PgCredential<'a> {
    /// A declared manifest auth method (the static/named-endpoint path — env-backed). The host
    /// resolves the auth method's env host-side; the plugin's `secret` grant no longer covers it.
    AuthPurpose(&'a str),
    /// An explicit credential reference string (`scheme/...`), resolved through the host broker
    /// (cross-plugin grant + audit apply) — the discovered-endpoint path with an explicit ref.
    CredentialRef(&'a str),
    /// A discovered endpoint reference (`@endpoint/<id>`); the host materializes the credential the
    /// endpoint record carries (cross-plugin grant + audit apply).
    EndpointRef(&'a str),
}

/// The negotiated, non-secret connection parameters from a host-terminated handshake
/// ([`Host::conn_authenticate`]). Carries no credential.
pub struct HandshakeInfo {
    /// The server version reported via `ParameterStatus`, when present.
    pub server_version: Option<String>,
    /// All `ParameterStatus` values the server sent during startup.
    pub parameters: HashMap<String, String>,
}

/// Metadata for a stored blob (from [`Host::blob_info`]).
pub struct BlobInfo {
    /// The name the blob was stored under.
    pub name: String,
    /// Size in bytes.
    pub size: usize,
    /// The content's sha256 (also the `blob_ref`).
    pub sha256: String,
}

/// A blocking [`std::io::Read`] + [`std::io::Write`] adapter over an open host connection
/// ([`Host::conn_dial`]). Lets a plugin run a hand-rolled wire protocol — a minimal SQL client, the
/// PostgreSQL/MySQL wire protocols or HTTP/1.1 over the Docker unix socket — on top of standard buffered IO
/// (`BufReader::new(stream)`, `read_line`, `write_all`, …), while every byte still crosses the guarded
/// `conn.*` host capability. `read` returns `Ok(0)` at EOF. Usage: `conn_dial` to get the id, scope a
/// `ConnStream` for the exchange, then [`Host::conn_close`] the id once the stream is dropped.
///
/// An optional **per-read deadline** ([`ConnStream::set_read_deadline`], D-45) is forwarded to the
/// host's `conn.read` as `timeout_ms`: on elapsed the host returns a [`std::io::ErrorKind::TimedOut`]
/// (the connection stays open — the plugin decides to retry or close) instead of hanging.
pub struct ConnStream<'h, 'a> {
    host: &'h mut Host<'a>,
    conn_id: u64,
    read_deadline: Option<std::time::Duration>,
}

impl<'h, 'a> ConnStream<'h, 'a> {
    /// Wrap an open `conn_id` (from [`Host::conn_dial`]) as a blocking byte stream.
    pub fn new(host: &'h mut Host<'a>, conn_id: u64) -> Self {
        Self {
            host,
            conn_id,
            read_deadline: None,
        }
    }

    /// The underlying connection id.
    pub fn conn_id(&self) -> u64 {
        self.conn_id
    }

    /// Set the per-read deadline forwarded to the host's `conn.read` as `timeout_ms` (D-45).
    /// `None` clears it (unbounded, the default). On elapsed, [`read`](std::io::Read::read)
    /// returns [`std::io::ErrorKind::TimedOut`] without closing the connection.
    pub fn set_read_deadline(&mut self, deadline: Option<std::time::Duration>) {
        self.read_deadline = deadline;
    }
}

impl std::io::Read for ConnStream<'_, '_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let timeout_ms = self
            .read_deadline
            .map(|d| d.as_millis().min(u64::MAX as u128) as u64);
        let data = self
            .host
            .conn_read_timed(self.conn_id, buf.len(), timeout_ms)
            .map_err(|e| {
                // Surface a host timeout as ErrorKind::TimedOut so a wire-protocol loop can
                // distinguish it from a clean EOF (Ok(0)) or a hard read error.
                if e.contains("timed out") {
                    std::io::Error::new(std::io::ErrorKind::TimedOut, e)
                } else {
                    std::io::Error::other(e)
                }
            })?;
        let n = data.len().min(buf.len());
        buf[..n].copy_from_slice(&data[..n]);
        Ok(n)
    }
}

impl std::io::Write for ConnStream<'_, '_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.host
            .conn_write(self.conn_id, buf)
            .map_err(std::io::Error::other)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct PreparedInput {
    normalized: Value,
    typed: Box<dyn std::any::Any + Send>,
}

type PrepareFn = Box<dyn Fn(Value) -> Result<PreparedInput, String> + Send + Sync>;
type OpFn = Box<dyn Fn(PreparedInput, &mut Host) -> Result<Value, String> + Send + Sync>;

struct OpHandler {
    prepare: PrepareFn,
    call: OpFn,
}

/// A custom preflight rule for one operation: `input -> problems` (empty = valid). Runs alongside
/// the generic [`schema_preflight`] in both the `--dry-run` path (via [`VALIDATE_OP`]) and runtime
/// dispatch — see [`PluginBuilder::preflight`].
type PreflightFn = Box<dyn Fn(&Value) -> Vec<String> + Send + Sync>;

/// Collects a manifest + op handlers, then [`serve`](Plugin::serve)s them over the plugin protocol.
pub struct PluginBuilder {
    manifest: PluginManifest,
    ops: HashMap<String, OpHandler>,
    preflights: HashMap<String, PreflightFn>,
    registration_errors: Vec<String>,
}

impl PluginBuilder {
    /// Start a plugin named `name` at `version`.
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            manifest: PluginManifest {
                name: name.into(),
                version: version.into(),
                ..Default::default()
            },
            ops: HashMap::new(),
            preflights: HashMap::new(),
            registration_errors: Vec::new(),
        }
    }

    fn operation_collision(&self, spec: &OperationSpec) -> Option<String> {
        let public_name = spec.projected_name(&self.manifest.name);
        self.manifest.operations.iter().find_map(|existing| {
            if existing.name == spec.name {
                let identical = serde_json::to_value(existing).ok()
                    == serde_json::to_value(spec).ok();
                let shape = if identical { "identical" } else { "conflicting" };
                return Some(format!(
                    "duplicate plugin operation `{}` ({shape} declaration; first effects/risk: {:?}/{:?}, second: {:?}/{:?})",
                    spec.name, existing.effects, existing.risk, spec.effects, spec.risk
                ));
            }
            let existing_public = existing.projected_name(&self.manifest.name);
            (existing_public == public_name).then(|| {
                format!(
                    "plugin operations `{}` and `{}` both project as public operation `{public_name}`",
                    existing.name, spec.name
                )
            })
        })
    }

    /// Declare the host capabilities this plugin needs (process/secret/http).
    pub fn capabilities(mut self, caps: Caps) -> Self {
        self.manifest.capabilities = caps;
        self
    }

    /// Add an auth method (resolved by purpose from env).
    pub fn auth(mut self, method: AuthMethod) -> Self {
        self.manifest.auth.push(method);
        self
    }

    /// Add a configurable endpoint (base URL from env, or a host-composed template), addressed by
    /// the plugin **by reference** via the `*_ref` IO helpers.
    pub fn endpoint(mut self, ep: EndpointSpec) -> Self {
        self.manifest.endpoints.push(ep);
        self
    }

    /// Declare a **non-secret** config value (D-32) readable via [`Host::config`]. The host refuses
    /// undeclared names and secret-classified env keys.
    pub fn config(mut self, spec: ConfigSpec) -> Self {
        self.manifest.config.push(spec);
        self
    }

    /// Declare a datasource this plugin contributes records for.
    pub fn datasource(mut self, decl: Declaration) -> Self {
        self.manifest.datasources.push(decl);
        self
    }

    /// Declare a model-catalog operation group owned by this plugin.
    pub fn group(mut self, group: ToolGroup) -> Self {
        self.manifest.groups.push(group);
        self
    }

    /// Declare a product this plugin can **discover** endpoints for as a provider (D-26). The host's
    /// fan-out broker routes a consumer's discovery query for this product to this plugin's
    /// `endpoint.discover` op. Call once per product.
    pub fn discovers(mut self, product: impl Into<String>) -> Self {
        self.manifest.discovers.push(product.into());
        self
    }

    /// Deprecated value-only registration spelling.
    ///
    /// Use [`operation_typed`](Self::operation_typed) for closed contracts. Use
    /// [`operation_flexible`](Self::operation_flexible) only when an operation deliberately accepts
    /// or returns an open `serde_json::Value` payload.
    #[deprecated(
        since = "0.24.0",
        note = "use operation_typed for closed contracts; use operation_flexible only for intentionally open Value payloads"
    )]
    pub fn operation(
        self,
        spec: OperationSpec,
        handler: impl Fn(Value, &mut Host) -> Result<Value, String> + Send + Sync + 'static,
    ) -> Self {
        self.operation_flexible(spec, handler)
    }

    /// Explicit compatibility adapter for open/flex payloads whose executable contract is
    /// intentionally `serde_json::Value`.
    ///
    /// Closed-shape operations should use [`operation_typed`](Self::operation_typed). This adapter
    /// is for transitional handlers and vendor payload pass-through where a stable Rust output type
    /// would erase meaningful vendor fields.
    pub fn operation_flexible(
        mut self,
        spec: OperationSpec,
        handler: impl Fn(Value, &mut Host) -> Result<Value, String> + Send + Sync + 'static,
    ) -> Self {
        if let Some(error) = self.operation_collision(&spec) {
            self.registration_errors.push(error);
            return self;
        }
        self.ops.insert(
            spec.name.clone(),
            OpHandler {
                prepare: Box::new(|input| {
                    Ok(PreparedInput {
                        normalized: input.clone(),
                        typed: Box::new(input),
                    })
                }),
                call: Box::new(move |prepared, host| {
                    let input = prepared.typed.downcast::<Value>().map_err(|_| {
                        "host-kit internal error: flexible input type mismatch".to_string()
                    })?;
                    handler(*input, host)
                }),
            },
        );
        self.manifest.operations.push(spec);
        self
    }

    /// Register an operation through the default typed path: its Rust input/output types are the
    /// executable and catalog contract. Input is deserialized exactly once with a field path on
    /// error, normalized by serializing that typed value, and then used for schema/custom preflight
    /// and the typed handler. The successful output is serialized once and its schema is projected
    /// into the manifest.
    pub fn operation_typed<I, O>(
        mut self,
        mut spec: OperationSpec,
        handler: impl Fn(I, &mut Host) -> Result<O, String> + Send + Sync + 'static,
    ) -> Self
    where
        I: DeserializeOwned + Serialize + schemars::JsonSchema + Send + 'static,
        O: Serialize + schemars::JsonSchema + 'static,
    {
        spec.input_schema = op_input_schema::<I>();
        spec.output_schema = Some(op_output_schema::<O>());
        if let Some(error) = self.operation_collision(&spec) {
            self.registration_errors.push(error);
            return self;
        }
        let operation = spec.name.clone();
        self.ops.insert(
            operation.clone(),
            OpHandler {
                prepare: Box::new(move |input| {
                    let typed: I = serde_path_to_error::deserialize(input).map_err(|error| {
                        let path = error.path().to_string();
                        let location = if path.is_empty() {
                            "<input>".to_string()
                        } else {
                            path
                        };
                        format!(
                            "invalid typed input for `{operation}` at `{location}`: {}",
                            error.inner()
                        )
                    })?;
                    let normalized = serde_json::to_value(&typed).map_err(|error| {
                        format!("normalize typed input for `{operation}`: {error}")
                    })?;
                    Ok(PreparedInput {
                        normalized,
                        typed: Box::new(typed),
                    })
                }),
                call: Box::new(move |prepared, host| {
                    let input = prepared.typed.downcast::<I>().map_err(|_| {
                        "host-kit internal error: typed input type mismatch".to_string()
                    })?;
                    let output = handler(*input, host)?;
                    serde_json::to_value(output)
                        .map_err(|error| format!("serialize typed operation result: {error}"))
                }),
            },
        );
        self.manifest.operations.push(spec);
        self
    }

    /// Attach a **custom preflight rule** to a registered operation (D-88), for constraints the
    /// JSON schema cannot express: conditional targets (`ref` OR `project`+`iid`), alias
    /// requirements, regex compilation, empty-update guards. The rule runs *in addition to* the
    /// generic [`schema_preflight`] every op gets, in both the `--dry-run` path (via the
    /// auto-registered [`VALIDATE_OP`]) and runtime dispatch — so the two verdicts can never
    /// disagree. Return every problem found (empty = valid).
    pub fn preflight(
        mut self,
        op: impl Into<String>,
        rule: impl Fn(&Value) -> Vec<String> + Send + Sync + 'static,
    ) -> Self {
        let op = op.into();
        match self.preflights.entry(op) {
            std::collections::hash_map::Entry::Occupied(entry) => self.registration_errors.push(
                format!("duplicate preflight rule for operation `{}`", entry.key()),
            ),
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(Box::new(rule));
            }
        }
        self
    }

    /// Return a clone of the manifest accumulated so far, useful for plugin manifest tests.
    pub fn manifest(&self) -> PluginManifest {
        self.manifest.clone()
    }

    /// Transform every operation declaration accumulated so far. This is useful for generated
    /// manifest metadata (for example output schemas sourced from an external API contract) that
    /// should stay separate from hand-written handler registration. The operation names and
    /// handler map are unaffected.
    pub fn map_operations(mut self, mut map: impl FnMut(&mut OperationSpec)) -> Self {
        for operation in &mut self.manifest.operations {
            map(operation);
        }
        self
    }

    /// Finish building without serving, rejecting every manifest/handler identity mismatch.
    ///
    /// This is the production assembly path. It reports duplicate raw names, duplicate projected
    /// public names, conflicting handler metadata, missing handlers, and invalid preflight targets
    /// before a subprocess can publish its manifest.
    pub fn try_build(self) -> Result<Plugin, String> {
        let plugin_name = self.manifest.name.clone();
        let mut problems = self.registration_errors;
        for op in self.preflights.keys() {
            if !self.ops.contains_key(op) {
                problems.push(format!("preflight rule for unregistered operation `{op}`"));
            }
        }
        let mut manifest = self.manifest;
        if !manifest.operations.iter().any(|o| o.name == VALIDATE_OP) {
            manifest.operations.push(internal_op(
                VALIDATE_OP,
                "Validate an operation input without executing it: {operation, input} -> \
                 {operation, valid, problems}. The host's --dry-run path calls this so local \
                 validation and runtime dispatch share one preflight verdict.",
                json!({
                    "type": "object",
                    "properties": {
                        "operation": { "type": "string", "description": "The op name to validate against." },
                        "input": { "type": "object", "description": "The op input to validate." },
                    },
                    "required": ["operation"],
                }),
            ));
        }

        if let Err(error) = flux_plugin_protocol::validate_manifest_operations(&manifest) {
            problems.push(error);
        }
        for operation in &manifest.operations {
            let auto_validate =
                operation.name == VALIDATE_OP && !self.ops.contains_key(VALIDATE_OP);
            if !auto_validate && !self.ops.contains_key(&operation.name) {
                problems.push(format!(
                    "manifest operation `{}` has no registered handler",
                    operation.name
                ));
            }
        }
        for operation in self.ops.keys() {
            if !manifest
                .operations
                .iter()
                .any(|spec| spec.name == *operation)
            {
                problems.push(format!(
                    "handler operation `{operation}` has no manifest declaration"
                ));
            }
        }
        if !problems.is_empty() {
            problems.sort();
            problems.dedup();
            return Err(format!(
                "plugin `{plugin_name}` assembly failed:\n  - {}",
                problems.join("\n  - ")
            ));
        }

        Ok(Plugin {
            manifest,
            ops: self.ops,
            preflights: self.preflights,
        })
    }

    /// Compatibility wrapper for callers that cannot yet return a build error.
    ///
    /// # Deprecated
    ///
    /// Use [`try_build`](Self::try_build); panicking hides the plugin and operation source from a
    /// host that could otherwise render an actionable startup error.
    pub fn build(self) -> Plugin {
        self.try_build()
            .unwrap_or_else(|error| panic!("plugin assembly failed: {error}"))
    }

    /// Fallibly build and run the stdio serve loop (call from `main`).
    pub fn try_serve(self) -> Result<(), String> {
        let plugin = self.try_build()?;
        flux_plugin_protocol::serve(plugin);
        Ok(())
    }

    /// Compatibility wrapper that panics on an invalid plugin declaration.
    ///
    /// # Deprecated
    ///
    /// Plugin binaries should return [`try_serve`](Self::try_serve) from `main`.
    #[deprecated(note = "use PluginBuilder::try_serve and return its error from main")]
    pub fn serve(self) {
        self.try_serve()
            .unwrap_or_else(|error| panic!("plugin assembly failed: {error}"));
    }
}

/// A built plugin: a [`PluginHandler`] dispatching to the registered op closures.
pub struct Plugin {
    manifest: PluginManifest,
    ops: HashMap<String, OpHandler>,
    preflights: HashMap<String, PreflightFn>,
}

impl Plugin {
    /// Resolve a public model-facing compatibility name back to the stable subprocess handler
    /// identity. Exact raw names win so existing `flux plugin call` invocations remain valid.
    fn raw_operation_name<'a>(&'a self, operation: &str) -> Option<&'a str> {
        self.ops
            .get_key_value(operation)
            .map(|(name, _)| name.as_str())
            .or_else(|| {
                self.manifest
                    .operations
                    .iter()
                    .find(|spec| {
                        spec.projected_name(&self.manifest.name) == operation
                            && self.ops.contains_key(&spec.name)
                    })
                    .map(|spec| spec.name.as_str())
            })
    }

    /// The combined preflight verdict for one op input (D-88): the generic [`schema_preflight`]
    /// against the op's declared `input_schema`, plus any custom
    /// [`preflight`](PluginBuilder::preflight) rule. This is what runtime dispatch enforces and
    /// what [`VALIDATE_OP`] answers — the single source of the dry-run/runtime verdict.
    pub fn validate_input(&self, operation: &str, input: &Value) -> preflight::PreflightReport {
        let Some(raw_operation) = self.raw_operation_name(operation) else {
            return preflight::PreflightReport::default();
        };
        self.prepare_input(raw_operation, input.clone()).0
    }

    fn prepare_input(
        &self,
        raw_operation: &str,
        input: Value,
    ) -> (preflight::PreflightReport, Option<PreparedInput>) {
        let mut report = preflight::PreflightReport::default();
        let Some(handler) = self.ops.get(raw_operation) else {
            return (report, None);
        };
        let prepared = match (handler.prepare)(input) {
            Ok(prepared) => prepared,
            Err(error) => {
                report.problems.push(error);
                return (report, None);
            }
        };
        if let Some(spec) = self
            .manifest
            .operations
            .iter()
            .find(|o| o.name == raw_operation)
        {
            report = schema_preflight(&spec.input_schema, &prepared.normalized);
        }
        if let Some(rule) = self.preflights.get(raw_operation) {
            report.problems.extend(rule(&prepared.normalized));
        }
        (report, Some(prepared))
    }
}

impl PluginHandler for Plugin {
    fn manifest(&self) -> PluginManifest {
        self.manifest.clone()
    }

    fn call(
        &self,
        operation: &str,
        input: Value,
        host: &mut dyn GuestHost,
    ) -> Result<Value, String> {
        // The auto-registered validate op (D-88): answer with the preflight verdict, never
        // executing anything. A plugin that registered its own op under this name wins below.
        if operation == VALIDATE_OP && !self.ops.contains_key(VALIDATE_OP) {
            let target = input
                .get("operation")
                .and_then(|v| v.as_str())
                .ok_or("plugin.validate: `operation` (string) required")?;
            let raw_target = self
                .raw_operation_name(target)
                .ok_or_else(|| format!("unknown operation: {target}"))?;
            let op_input = input.get("input").cloned().unwrap_or_else(|| json!({}));
            let report = self.validate_input(raw_target, &op_input);
            return Ok(json!({
                "operation": target,
                "valid": report.problems.is_empty(),
                "problems": report.problems,
                "warnings": report.warnings,
            }));
        }
        let raw_operation = self
            .raw_operation_name(operation)
            .ok_or_else(|| format!("unknown operation: {operation}"))?;
        // Runtime dispatch runs the same preflight the dry-run path sees, so the two verdicts
        // can never disagree (D-88). Warnings stay advisory — only problems block dispatch.
        let (report, prepared) = self.prepare_input(raw_operation, input);
        let problems = report.problems;
        if !problems.is_empty() {
            return Err(format!(
                "invalid input for `{operation}` ({} problem(s)):\n  - {}",
                problems.len(),
                problems.join("\n  - ")
            ));
        }
        let prepared = prepared.ok_or_else(|| format!("unknown operation: {operation}"))?;
        let op = self
            .ops
            .get(raw_operation)
            .ok_or_else(|| format!("unknown operation: {operation}"))?;
        let mut h = Host { inner: host };
        (op.call)(prepared, &mut h)
    }
}

/// A simple read-only operation spec helper (Effect::Read, low risk, idempotent).
///
/// Everything this helper does not name takes [`OperationSpec`]'s own `Default`, which is by
/// construction the same value a manifest omitting the field deserializes to (every field carries
/// `#[serde(default)]`). The pack must not carry a second exhaustive-literal tripwire for wire
/// additions: the designated one is `wire_contract.rs` in the protocol crate, where it fires at the
/// author's desk instead of in the separate `plugins:` CI job.
pub fn read_op(name: &str, description: &str, input_schema: Value) -> OperationSpec {
    OperationSpec {
        name: name.into(),
        description: description.into(),
        input_schema,
        effects: vec![Effect::Read],
        risk: Some(Risk::Low),
        idempotency: Some(Idempotency::Idempotent),
        ..OperationSpec::default()
    }
}

/// A write/mutating operation spec helper (Effect::Write, medium risk, non-idempotent).
///
/// Unnamed fields take `Default` — see [`read_op`] for why the pack does not spell them out.
pub fn write_op(name: &str, description: &str, input_schema: Value) -> OperationSpec {
    OperationSpec {
        name: name.into(),
        description: description.into(),
        input_schema,
        effects: vec![Effect::Write, Effect::Network],
        risk: Some(Risk::Medium),
        idempotency: Some(Idempotency::NonIdempotent),
        ..OperationSpec::default()
    }
}

/// A **typed** read-only op: `input_schema` is derived from `T` via `schemars`
/// ([`op_input_schema`]) instead of a hand-written `json!({...})` object.
///
/// For the default [`PluginBuilder::operation_typed`] path, `T` should derive
/// `Deserialize + Serialize + schemars::JsonSchema`; its fields are the executable input contract.
/// Use `Option<T>` for optional fields and serde aliases/defaults for accepted compatibility
/// spellings. Prefer `#[serde(deny_unknown_fields)]` on closed contracts. Effects/risk/idempotency
/// match [`read_op`] (Read, Low, Idempotent).
pub fn read_op_typed<T: schemars::JsonSchema + 'static>(
    name: &str,
    description: &str,
) -> OperationSpec {
    read_op(name, description, op_input_schema::<T>())
}

/// A **typed** write/mutating op: `input_schema` derived from `T` via `schemars`
/// ([`op_input_schema`]). Effects/risk/idempotency match [`write_op`] (Write+Network,
/// Medium, NonIdempotent). See [`read_op_typed`] for the `T` contract.
pub fn write_op_typed<T: schemars::JsonSchema + 'static>(
    name: &str,
    description: &str,
) -> OperationSpec {
    write_op(name, description, op_input_schema::<T>())
}

/// A **host-only** op (C-09a): not advertised to the LLM as a callable tool. The canonical case is
/// the `aws-bedrock` plugin's `auth` op, which returns raw AWS credentials — the model must never
/// call it, or the keys would appear in the tool result. The op stays dispatchable by the host via
/// the shared `PluginHost` handle; [`flux_plugin_protocol::visible_ops`] excludes it from the projected tool
/// catalog. Effects default to `Process`+`Network` (the conservative authorization floor
/// [`flux_plugin_protocol::PluginTool::new`] applies to an undeclared op) — override via the returned spec.
/// Unnamed fields take `Default` — see [`read_op`] for why the pack does not spell them out.
pub fn internal_op(name: &str, description: &str, input_schema: Value) -> OperationSpec {
    OperationSpec {
        name: name.into(),
        description: description.into(),
        input_schema,
        effects: Vec::new(),
        risk: Some(Risk::Low),
        idempotency: Some(Idempotency::Idempotent),
        internal: true,
        ..OperationSpec::default()
    }
}

/// Assign an operation to a plugin-declared operation group.
pub fn grouped(mut op: OperationSpec, group: &str) -> OperationSpec {
    op.group = Some(group.into());
    op
}

/// Expose an operation under a stable model-facing compatibility name while retaining its raw
/// subprocess/CLI dispatch identity.
pub fn exposed_as(mut op: OperationSpec, public_name: &str) -> OperationSpec {
    op.public_name = Some(public_name.into());
    op
}

/// Override an operation's risk classification.
pub fn risked(mut op: OperationSpec, risk: Risk) -> OperationSpec {
    op.risk = Some(risk);
    op
}

/// Declare an operation's per-op `process` narrowing (C-90): the argv **prefixes** this operation
/// may pass to the host's `process.run`/`process.spawn` capability (e.g. `&["kubectl get"]`).
/// Each prefix must sit inside the manifest-level `process` grant (manifest validation rejects it
/// otherwise); the host enforces the narrowing at callback time and projects it as the op's
/// `process.exec` authority, so a read op declared `kubectl get` both prompts as and is
/// structurally unable to run anything else.
pub fn with_process(mut op: OperationSpec, prefixes: &[&str]) -> OperationSpec {
    op.process = prefixes.iter().map(|s| (*s).to_string()).collect();
    op
}

/// Attach a manually authored JSON Schema describing an operation's successful result.
///
/// This is the compatibility path for [`PluginBuilder::operation_flexible`], where no closed Rust
/// output type exists. [`PluginBuilder::operation_typed`] derives the successful-result schema from
/// its output type and does not need this combinator. The schema is projected unchanged onto the
/// runtime `ToolSpec` and used by generated references (D-164).
pub fn with_output_schema(mut op: OperationSpec, output_schema: Value) -> OperationSpec {
    op.output_schema = Some(output_schema);
    op
}

/// A force-on group for organizing plugin operations. Empty `surface_when` means the group is active
/// whenever the plugin is loaded, so grouping does not hide installed plugin ops.
pub fn op_group(name: &str, description: &str, tools: &[&str]) -> ToolGroup {
    ToolGroup {
        name: name.into(),
        description: description.into(),
        tools: tools.iter().map(|s| (*s).into()).collect(),
        surface_when: Vec::new(),
    }
}

/// A standard datasource declaration for plugins that contribute records to the host index.
pub fn ds(name: &str, entity: &str, desc: &str) -> Declaration {
    Declaration {
        name: name.into(),
        entity: entity.into(),
        description: Some(desc.into()),
        capabilities: vec!["search".into(), "get".into(), "index".into()],
        entity_schema: None,
    }
}

/// A **typed** host-only op: `input_schema` derived from `T` via `schemars` ([`op_input_schema`]).
/// See [`internal_op`] for the host-only contract.
pub fn internal_op_typed<T: schemars::JsonSchema + 'static>(
    name: &str,
    description: &str,
) -> OperationSpec {
    internal_op(name, description, op_input_schema::<T>())
}

// ---------------------------------------------------------------------------
// Test support — a mock GuestHost so plugin op handlers can be unit-tested with no subprocess/network.
// ---------------------------------------------------------------------------

/// A scripted [`GuestHost`] for tests: returns canned results per host command. `http.do` matches by a
/// substring of the request URL.
pub struct MockHost {
    /// `(url-substring) -> JSON result for http.do` (matched in insertion order).
    pub http: Vec<(String, Value)>,
    /// `purpose -> secret value`.
    pub secrets: HashMap<String, String>,
    /// `config name -> value` for the gated non-secret `config` capability (D-32).
    pub configs: HashMap<String, String>,
    /// `endpoint_ref -> base url` for ref-based IO (`http.do`/`conn.dial` with an `endpoint_ref`).
    /// Covers both named (`svc.endpoint`) and discovered (`@endpoint/<id>`) refs — the resolver the
    /// real host installs. `http_ref`/`conn_dial_ref` resolve against this map.
    pub endpoint_refs: HashMap<String, String>,
    /// `credential_ref` OR `endpoint_ref` -> materialized value, for the gated `credential` host
    /// capability (the password a raw-socket plugin needs for in-band auth).
    pub credentials: HashMap<String, String>,
    /// `(argv-substring) -> stdout string for process.run` (matched in insertion order).
    pub process: Vec<(String, String)>,
    /// The `proc_id` returned by every `process.spawn`.
    pub spawn_proc_id: u64,
    /// Canned `process.read` output `(stdout, stderr)`.
    pub proc_output: (String, String),
    /// Liveness reported by `process.read` / `process.status`.
    pub proc_running: bool,
    /// Exit code reported once not running.
    pub proc_exit_code: Option<i64>,
    /// `(url-substring) -> raw bytes` for a binary `http.do` (response_binary), matched in insertion order.
    pub http_bytes: Vec<(String, Vec<u8>)>,
    /// A FIFO queue of `(url-substring, JSON)` responses drained one-per-`http.do` call (first
    /// matching entry popped), for tests that hit the **same URL more than once** and need
    /// different responses per call (e.g. a seed search then a fan-out search on the same path).
    /// Checked before [`http`](MockHost::http); falls back to `http`'s first-match when empty.
    pub http_seq: std::cell::RefCell<Vec<(String, Value)>>,
    /// `(url-substring, status, body)` canned responses with a custom status code (for error
    /// paths). Checked first (before `http_seq`/`http`); first substring match wins.
    pub http_status: Vec<(String, u16, String)>,
    /// Records the plugin contributed (captured for assertions).
    pub contributed: std::cell::RefCell<Vec<Record>>,
    /// An in-memory `conn.*` byte buffer: `conn.write` appends, `conn.read` drains (a loopback echo).
    pub conn_buf: std::cell::RefCell<Vec<u8>>,
    /// Canned server bytes the next `conn.read`s return (FIFO, one chunk per call). When non-empty it
    /// takes priority over the loopback echo — the simulated server side of a `conn.*` exchange, so a
    /// hand-rolled wire-protocol client (SQL/Docker) can be tested without a real socket.
    pub conn_script: std::cell::RefCell<std::collections::VecDeque<Vec<u8>>>,
    /// An in-memory `blob.*` store: `blob_ref -> (name, bytes)`.
    pub blobs: std::cell::RefCell<HashMap<String, (String, Vec<u8>)>>,
    /// The `server_version` a host-terminated `conn.authenticate` reports back (D-31). Default
    /// `"16.2"`; override with [`with_pg_server_version`](MockHost::with_pg_server_version).
    pub pg_server_version: String,
    /// A log of every `host_call` the plugin made: `(command, payload)`, in call order. Lets a test
    /// assert what the plugin did and did NOT ask the host to do — e.g. that a host-terminated PG
    /// path never calls `credential`/`secret` and never puts a password on the wire (D-31).
    pub calls: std::cell::RefCell<Vec<(String, Value)>>,
}

impl Default for MockHost {
    fn default() -> Self {
        Self {
            http: Vec::new(),
            secrets: HashMap::new(),
            configs: HashMap::new(),
            endpoint_refs: HashMap::new(),
            credentials: HashMap::new(),
            process: Vec::new(),
            spawn_proc_id: 1,
            proc_output: (String::new(), String::new()),
            proc_running: false,
            proc_exit_code: None,
            http_bytes: Vec::new(),
            http_seq: std::cell::RefCell::new(Vec::new()),
            http_status: Vec::new(),
            contributed: std::cell::RefCell::new(Vec::new()),
            conn_buf: std::cell::RefCell::new(Vec::new()),
            conn_script: std::cell::RefCell::new(std::collections::VecDeque::new()),
            blobs: std::cell::RefCell::new(HashMap::new()),
            pg_server_version: "16.2".to_string(),
            calls: std::cell::RefCell::new(Vec::new()),
        }
    }
}

impl MockHost {
    /// Canned JSON response for any `http.do` whose URL contains `url_substr`.
    pub fn with_http(mut self, url_substr: &str, result: Value) -> Self {
        self.http.push((url_substr.into(), result));
        self
    }
    /// A canned `http.do` response with a custom **status code** + raw string body (not JSON),
    /// for testing error paths (e.g. a 503 from a readiness endpoint). Matched by URL substring,
    /// first-match like [`with_http`](MockHost::with_http); checked before `http_seq`/`http`.
    pub fn with_http_status_body(mut self, url_substr: &str, status: u16, body: &str) -> Self {
        self.http_status
            .push((url_substr.into(), status, body.to_string()));
        self
    }
    /// A sequential canned JSON response: the first `http.do` whose URL contains `url_substr`
    /// pops and returns this, then it's gone. Use for tests that hit the same URL multiple
    /// times with different responses (e.g. seed search then fan-out search).
    pub fn with_http_seq(self, url_substr: &str, result: Value) -> Self {
        self.http_seq.borrow_mut().push((url_substr.into(), result));
        self
    }
    /// A readable non-secret config value for the gated `config` capability (D-32).
    pub fn with_config(mut self, name: &str, value: &str) -> Self {
        self.configs.insert(name.into(), value.into());
        self
    }
    /// A resolvable endpoint **reference** (named or discovered `@endpoint/<id>`) → base URL, for
    /// the ref-based `http_ref`/`conn_dial_ref` paths the real host resolves through the broker.
    pub fn with_endpoint_ref(mut self, endpoint_ref: &str, url: &str) -> Self {
        self.endpoint_refs.insert(endpoint_ref.into(), url.into());
        self
    }
    /// A materialized credential for the gated `credential` host capability, keyed by EITHER a
    /// `credential_ref` string or an `endpoint_ref` — whichever the plugin passes.
    pub fn with_credential(mut self, key: &str, value: &str) -> Self {
        self.credentials.insert(key.into(), value.into());
        self
    }
    /// A resolvable secret purpose.
    pub fn with_secret(mut self, purpose: &str, value: &str) -> Self {
        self.secrets.insert(purpose.into(), value.into());
        self
    }
    /// Canned stdout for any `process.run` whose joined argv contains `argv_substr`.
    pub fn with_process(mut self, argv_substr: &str, stdout: &str) -> Self {
        self.process.push((argv_substr.into(), stdout.into()));
        self
    }
    /// The `proc_id` every `process.spawn` returns.
    pub fn with_spawn(mut self, proc_id: u64) -> Self {
        self.spawn_proc_id = proc_id;
        self
    }
    /// Canned `process.read` output + liveness.
    pub fn with_proc_output(mut self, stdout: &str, stderr: &str, running: bool) -> Self {
        self.proc_output = (stdout.into(), stderr.into());
        self.proc_running = running;
        self
    }
    /// Canned raw bytes for any binary `http.do` (response_binary) whose URL contains `url_substr`.
    pub fn with_http_bytes(mut self, url_substr: &str, bytes: Vec<u8>) -> Self {
        self.http_bytes.push((url_substr.into(), bytes));
        self
    }
    /// Queue canned server bytes the next `conn.read`(s) return (FIFO, one chunk per call) — the
    /// simulated server side of a `conn.*` exchange, for testing a hand-rolled wire-protocol client.
    pub fn with_conn_response(mut self, bytes: impl Into<Vec<u8>>) -> Self {
        self.conn_script.get_mut().push_back(bytes.into());
        self
    }
    /// The `server_version` a host-terminated `conn.authenticate` reports back (D-31).
    pub fn with_pg_server_version(mut self, version: &str) -> Self {
        self.pg_server_version = version.into();
        self
    }
}

/// Join a base URL and a relative `path` with exactly one separating slash — a small stand-in for
/// the real host's `url::Url::join` over the SQL/HTTP-DSN shapes the tests exercise (avoids a `url`
/// dependency in the mock). An empty path returns the base unchanged.
fn join_url(base: &str, path: &str) -> String {
    if path.is_empty() {
        return base.to_string();
    }
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

impl GuestHost for MockHost {
    fn host_call(&mut self, command: &str, payload: Value) -> Result<Value, String> {
        // Log every call so a test can assert what the plugin did / did not ask of the host (D-31).
        self.calls
            .borrow_mut()
            .push((command.to_string(), payload.clone()));
        match command {
            "auth.available" => {
                let purpose = payload.get("purpose").and_then(Value::as_str).unwrap_or("");
                Ok(json!({ "available": self.secrets.contains_key(purpose) }))
            }
            "secret" => {
                let p = payload
                    .get("purpose")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                self.secrets
                    .get(p)
                    .map(|v| json!({ "value": v }))
                    .ok_or_else(|| format!("mock: no secret for purpose `{p}`"))
            }
            "config" => {
                // The gated non-secret config read (D-32). The real host additionally refuses
                // secret-classified env keys; the mock just resolves declared names.
                let n = payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
                self.configs
                    .get(n)
                    .map(|v| json!({ "value": v }))
                    .ok_or_else(|| format!("mock: no config `{n}`"))
            }
            "http.do" => {
                // Ref-based IO: resolve `endpoint_ref` to a base URL + join `path`, mirroring the
                // real host so a plugin's `http_ref` call matches against the same canned `http`/
                // `http_bytes` entries (by URL substring) as a `url`-based call.
                let url = if let Some(er) = payload.get("endpoint_ref").and_then(|v| v.as_str()) {
                    let base = self
                        .endpoint_refs
                        .get(er)
                        .cloned()
                        .ok_or_else(|| format!("mock: no endpoint_ref `{er}`"))?;
                    let path = payload.get("path").and_then(|v| v.as_str()).unwrap_or("");
                    join_url(&base, path)
                } else {
                    payload
                        .get("url")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string()
                };
                let url = url.as_str();
                // Custom-status canned responses (error paths) — checked first.
                if let Some((_, status, body)) = self
                    .http_status
                    .iter()
                    .find(|(sub, _, _)| url.contains(sub.as_str()))
                    .cloned()
                {
                    return Ok(json!({ "status": status, "body": body }));
                }
                // Sequential responses: drain the first matching entry, then fall back to
                // the first-match `http` table.
                let seq_pos = {
                    let seq = self.http_seq.borrow();
                    seq.iter().position(|(sub, _)| url.contains(sub.as_str()))
                };
                if let Some(pos) = seq_pos {
                    let (_, body) = self.http_seq.borrow_mut().remove(pos);
                    return Ok(
                        json!({ "status": 200, "body": serde_json::to_string(&body).unwrap() }),
                    );
                }
                // Binary download path: return base64 of canned raw bytes, matching the host.
                if payload
                    .get("response_binary")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
                {
                    let bytes = self
                        .http_bytes
                        .iter()
                        .find(|(sub, _)| url.contains(sub.as_str()))
                        .map(|(_, b)| b.clone())
                        .ok_or_else(|| format!("mock: no canned http_bytes for `{url}`"))?;
                    return Ok(json!({
                        "status": 200,
                        "body_b64": base64::engine::general_purpose::STANDARD.encode(&bytes),
                    }));
                }
                let body = self
                    .http
                    .iter()
                    .find(|(sub, _)| url.contains(sub.as_str()))
                    .map(|(_, v)| v.clone())
                    .ok_or_else(|| format!("mock: no canned http for `{url}`"))?;
                Ok(json!({ "status": 200, "body": serde_json::to_string(&body).unwrap() }))
            }
            "process.spawn" => Ok(json!({ "proc_id": self.spawn_proc_id })),
            "process.read" => {
                let mut v = json!({
                    "stdout": self.proc_output.0,
                    "stderr": self.proc_output.1,
                    "running": self.proc_running,
                });
                if let Some(code) = self.proc_exit_code {
                    v["exit_code"] = json!(code);
                }
                Ok(v)
            }
            "process.status" => {
                let mut v = json!({ "running": self.proc_running });
                if let Some(code) = self.proc_exit_code {
                    v["exit_code"] = json!(code);
                }
                Ok(v)
            }
            "process.kill" => Ok(json!({ "ok": true })),
            "process.run" => {
                let argv = payload
                    .get("argv")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str())
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default();
                let stdout = self
                    .process
                    .iter()
                    .find(|(sub, _)| argv.contains(sub.as_str()))
                    .map(|(_, out)| out.clone())
                    .ok_or_else(|| format!("mock: no canned process for `{argv}`"))?;
                Ok(json!({ "stdout": stdout, "stderr": "", "exit_code": 0 }))
            }
            "datasource.records" => {
                let recs: Vec<Record> =
                    serde_json::from_value(payload.get("records").cloned().unwrap_or(Value::Null))
                        .map_err(|e| e.to_string())?;
                let n = recs.len();
                self.contributed.borrow_mut().extend(recs);
                Ok(json!({ "indexed": n }))
            }
            "credential" => {
                // The gated `credential` host capability: materialize a credential value for the
                // trusted plugin's in-band auth. Keyed by EITHER `credential_ref` or `endpoint_ref`.
                let key = payload
                    .get("credential_ref")
                    .and_then(|v| v.as_str())
                    .or_else(|| payload.get("endpoint_ref").and_then(|v| v.as_str()))
                    .ok_or("mock: credential requires `credential_ref` or `endpoint_ref`")?;
                self.credentials
                    .get(key)
                    .map(|v| json!({ "value": v }))
                    .ok_or_else(|| format!("mock: no credential for `{key}`"))
            }
            "conn.dial" => {
                // A ref-based dial resolves the `endpoint_ref` (so a bad/unconfigured ref errors,
                // and the ref — not global state — drives which target a multi-instance plugin hits).
                if let Some(er) = payload.get("endpoint_ref").and_then(|v| v.as_str()) {
                    if !self.endpoint_refs.contains_key(er) {
                        return Err(format!("mock: no endpoint_ref `{er}` to dial"));
                    }
                }
                Ok(json!({ "conn_id": 1 }))
            }
            "conn.authenticate" => {
                // Host-terminated handshake (D-31): the HOST would speak the wire auth here. The mock
                // does not run the wire protocol; it only proves the guest-side contract — the plugin
                // passes a credential *location* (never a value) and gets back the negotiated
                // parameters. A discovered credential ref/endpoint ref must still be resolvable
                // host-side (looked up but NOT returned to the plugin); the static `auth_purpose`
                // path resolves from host env, so nothing plugin-visible is needed.
                if let Some(cr) = payload.get("credential_ref").and_then(|v| v.as_str()) {
                    if !self.credentials.contains_key(cr) {
                        return Err(format!("mock: no credential for `{cr}`"));
                    }
                } else if let Some(er) = payload.get("endpoint_ref").and_then(|v| v.as_str()) {
                    if !self.credentials.contains_key(er) {
                        return Err(format!("mock: no credential for endpoint `{er}`"));
                    }
                } else if payload
                    .get("auth_purpose")
                    .and_then(|v| v.as_str())
                    .is_none()
                {
                    return Err("mock: conn.authenticate requires a credential location".into());
                }
                Ok(json!({
                    "server_version": self.pg_server_version,
                    "parameters": { "server_version": self.pg_server_version },
                }))
            }
            "conn.write" => {
                let b64 = payload
                    .get("data_b64")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let data = base64::engine::general_purpose::STANDARD
                    .decode(b64)
                    .map_err(|e| e.to_string())?;
                let n = data.len();
                self.conn_buf.borrow_mut().extend(data);
                Ok(json!({ "written": n }))
            }
            "conn.read" => {
                let max = payload.get("max").and_then(|v| v.as_u64()).unwrap_or(65536) as usize;
                // Canned server responses (FIFO) take priority; fall back to the loopback echo.
                let mut script = self.conn_script.borrow_mut();
                let out: Vec<u8> = if let Some(front) = script.front_mut() {
                    let take = front.len().min(max);
                    let chunk: Vec<u8> = front.drain(..take).collect();
                    if front.is_empty() {
                        script.pop_front();
                    }
                    chunk
                } else {
                    let mut buf = self.conn_buf.borrow_mut();
                    let take = buf.len().min(max);
                    buf.drain(..take).collect()
                };
                // D-45: when a per-call deadline is set and no data was ready, surface a
                // timeout (the connection stays open) so a ConnStream surfaces ErrorKind::TimedOut.
                let timeout_ms = payload.get("timeout_ms").and_then(|v| v.as_u64());
                let timed_out = timeout_ms.is_some() && out.is_empty();
                Ok(json!({
                    "data_b64": base64::engine::general_purpose::STANDARD.encode(&out),
                    "eof": out.is_empty() && !timed_out,
                    "timed_out": timed_out
                }))
            }
            "conn.close" => Ok(json!({ "ok": true })),
            "blob.put" => {
                let name = payload
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let b64 = payload
                    .get("data_b64")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let data = base64::engine::general_purpose::STANDARD
                    .decode(b64)
                    .map_err(|e| e.to_string())?;
                let r = format!("mockblob-{}", self.blobs.borrow().len() + 1);
                self.blobs.borrow_mut().insert(r.clone(), (name, data));
                Ok(json!({ "blob_ref": r }))
            }
            "blob.get" => {
                let r = payload
                    .get("blob_ref")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let blobs = self.blobs.borrow();
                let (_, data) = blobs.get(r).ok_or_else(|| format!("mock: no blob {r}"))?;
                Ok(json!({ "data_b64": base64::engine::general_purpose::STANDARD.encode(data) }))
            }
            "blob.info" => {
                let r = payload
                    .get("blob_ref")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let blobs = self.blobs.borrow();
                let (name, data) = blobs.get(r).ok_or_else(|| format!("mock: no blob {r}"))?;
                Ok(json!({ "name": name, "size": data.len(), "sha256": r }))
            }
            other => Err(format!("mock: unknown command `{other}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
    #[serde(deny_unknown_fields)]
    struct TypedProfile {
        count: u32,
    }

    #[derive(Debug, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
    #[serde(deny_unknown_fields)]
    struct TypedSearchInput {
        #[serde(alias = "q")]
        query: String,
        profile: TypedProfile,
    }

    #[derive(Debug, serde::Serialize, schemars::JsonSchema)]
    struct TypedSearchOutput {
        summary: String,
        count: u32,
    }

    #[test]
    fn typed_operation_owns_schema_normalization_dispatch_and_output() {
        let plugin = PluginBuilder::new("acme", "0.1.0")
            .operation_typed::<TypedSearchInput, TypedSearchOutput>(
                read_op("acme.search", "typed search", json!({"type": "null"})),
                |input, _host| {
                    Ok(TypedSearchOutput {
                        summary: input.query,
                        count: input.profile.count,
                    })
                },
            )
            // The alias `q` is normalized to canonical `query` before this shared dry/live rule.
            .preflight("acme.search", |input| {
                if input.get("query").and_then(Value::as_str) == Some("ok") {
                    Vec::new()
                } else {
                    vec!["normalized `query` must be `ok`".into()]
                }
            })
            .build();

        let spec = plugin
            .manifest()
            .operations
            .into_iter()
            .find(|operation| operation.name == "acme.search")
            .unwrap();
        assert!(spec.input_schema["properties"].get("query").is_some());
        let output_schema = spec.output_schema.as_ref().unwrap();
        assert_eq!(output_schema["x-flux-type"], "TypedSearchOutput");
        assert!(output_schema["properties"].get("summary").is_some());

        let input = json!({"q": "ok", "profile": {"count": 3}});
        assert!(plugin
            .validate_input("acme.search", &input)
            .problems
            .is_empty());
        let mut host = MockHost::default();
        let output = plugin.call("acme.search", input, &mut host).unwrap();
        assert_eq!(output, json!({"summary": "ok", "count": 3}));
    }

    #[test]
    fn typed_drift_errors_are_path_aware_and_identical_in_dry_and_live_paths() {
        let plugin = PluginBuilder::new("acme", "0.1.0")
            .operation_typed::<TypedSearchInput, TypedSearchOutput>(
                read_op("acme.search", "typed search", json!({})),
                |input, _host| {
                    Ok(TypedSearchOutput {
                        summary: input.query,
                        count: input.profile.count,
                    })
                },
            )
            .build();
        let bad = json!({"query": "ok", "profile": {"count": "three"}});
        let dry = plugin.validate_input("acme.search", &bad);
        assert_eq!(dry.problems.len(), 1, "{dry:?}");
        assert!(dry.problems[0].contains("profile.count"), "{dry:?}");

        let mut host = MockHost::default();
        let live = plugin.call("acme.search", bad, &mut host).unwrap_err();
        assert!(live.contains(&dry.problems[0]), "dry={dry:?}; live={live}");
    }

    #[test]
    fn flexible_operation_is_an_explicit_open_payload_escape_hatch() {
        let plugin = PluginBuilder::new("acme", "0.1.0")
            .operation_flexible(
                read_op("acme.flex", "open", json!({"type": "object"})),
                |input, _host| Ok(input),
            )
            .build();
        let input = json!({"vendor_extension": {"anything": true}});
        let mut host = MockHost::default();
        assert_eq!(
            plugin.call("acme.flex", input.clone(), &mut host).unwrap(),
            input
        );
    }

    #[test]
    fn public_operation_name_dispatches_and_validates_the_raw_handler() {
        let plugin = PluginBuilder::new("websearch", "0.1.0")
            .operation_flexible(
                exposed_as(
                    read_op(
                        "websearch.search",
                        "search",
                        json!({
                            "type": "object",
                            "properties": { "query": { "type": "string" } },
                            "required": ["query"]
                        }),
                    ),
                    "web.search",
                ),
                |input, _host| Ok(json!({ "query": input["query"] })),
            )
            .build();
        let mut host = MockHost::default();

        let result = plugin
            .call("web.search", json!({ "query": "flux" }), &mut host)
            .unwrap();
        assert_eq!(result, json!({ "query": "flux" }));
        let validation = plugin
            .call(
                VALIDATE_OP,
                json!({ "operation": "web.search", "input": {} }),
                &mut host,
            )
            .unwrap();
        assert_eq!(validation["valid"], false);
        assert!(!validation["problems"].as_array().unwrap().is_empty());
    }

    #[test]
    fn builder_rejects_duplicate_handlers_with_conflicting_metadata() {
        let error = PluginBuilder::new("acme", "0.1.0")
            .operation_flexible(
                read_op("acme.thing", "read a thing", json!({"type": "object"})),
                |_input, _host| Ok(json!({"handler": "read"})),
            )
            .operation_flexible(
                write_op("acme.thing", "replace a thing", json!({"type": "object"})),
                |_input, _host| Ok(json!({"handler": "write"})),
            )
            .try_build()
            .err()
            .expect("duplicate handler must fail plugin assembly");
        assert!(error.contains("duplicate plugin operation `acme.thing`"));
        assert!(error.contains("conflicting declaration"));
        assert!(error.contains("Read"), "{error}");
        assert!(error.contains("Write"), "{error}");
    }

    #[test]
    fn builder_rejects_distinct_handlers_projecting_the_same_public_name() {
        let error = PluginBuilder::new("acme", "0.1.0")
            .operation_flexible(
                exposed_as(
                    read_op("acme.first", "first", json!({"type": "object"})),
                    "shared.lookup",
                ),
                |_input, _host| Ok(json!({"handler": "first"})),
            )
            .operation_flexible(
                exposed_as(
                    read_op("acme.second", "second", json!({"type": "object"})),
                    "shared.lookup",
                ),
                |_input, _host| Ok(json!({"handler": "second"})),
            )
            .try_build()
            .err()
            .expect("duplicate public operation must fail plugin assembly");
        assert!(error.contains("acme.first"), "{error}");
        assert!(error.contains("acme.second"), "{error}");
        assert!(error.contains("shared.lookup"), "{error}");
    }

    #[test]
    fn builder_rejects_manifest_handler_identity_drift() {
        let error = PluginBuilder::new("acme", "0.1.0")
            .operation_flexible(
                read_op("acme.thing", "read a thing", json!({"type": "object"})),
                |_input, _host| Ok(json!({"handler": "thing"})),
            )
            .map_operations(|operation| operation.name = "acme.renamed".into())
            .try_build()
            .err()
            .expect("manifest and handler identities must stay paired");

        assert!(
            error.contains("manifest operation `acme.renamed` has no registered handler"),
            "{error}"
        );
        assert!(
            error.contains("handler operation `acme.thing` has no manifest declaration"),
            "{error}"
        );
    }

    #[test]
    fn builder_dispatches_ops_and_host_calls_work() {
        let plugin = PluginBuilder::new("acme", "0.1.0")
            .capabilities(Caps {
                http: true,
                http_hosts: vec!["acme.example.com".into()],
                secrets: vec!["ACME_TOKEN".into()],
                ..Default::default()
            })
            .auth(AuthMethod {
                purpose: "api_token".into(),
                env: vec!["ACME_TOKEN".into()],
                description: String::new(),
                ..Default::default()
            })
            .endpoint(EndpointSpec {
                name: "acme.endpoint".into(),
                env: vec!["ACME_URL".into()],
                http_hosts: vec!["acme.example.com".into()],
                ..Default::default()
            })
            .operation_flexible(
                read_op("acme.thing", "fetch a thing", json!({"type": "object"})),
                |_input, host| {
                    // Ref-based IO: the endpoint is addressed by name; the host resolves the URL
                    // and performs the call (no URL-handback, D-32).
                    let v = host.get_json_ref("acme.endpoint", "things/1", Some("api_token"))?;
                    // contribute the fetched thing as a record
                    host.contribute(&[Record::new(
                        Source::new("acme"),
                        "acme.thing",
                        "1",
                        v.get("name").and_then(|n| n.as_str()).unwrap_or(""),
                        v.to_string(),
                    )])?;
                    Ok(v)
                },
            )
            .build();

        // manifest carries the op + auth + endpoint (+ the auto-registered validate op, D-88)
        let m = plugin.manifest();
        assert_eq!(m.operations.len(), 2);
        assert!(m.operations.iter().any(|o| o.name == VALIDATE_OP));
        assert_eq!(m.auth[0].purpose, "api_token");

        let mut host = MockHost::default()
            .with_endpoint_ref("acme.endpoint", "https://acme.test")
            .with_secret("api_token", "tok")
            .with_http("/things/1", json!({ "name": "Widget" }));
        let out = plugin
            .call("acme.thing", json!({}), &mut host)
            .expect("op runs");
        assert_eq!(out["name"], "Widget");
        // the op contributed a record
        assert_eq!(host.contributed.borrow().len(), 1);
        assert_eq!(host.contributed.borrow()[0].id, "1");

        // unknown op errors
        assert!(plugin.call("nope", json!({}), &mut host).is_err());
    }

    /// D-164: a plugin author can attach an operation's result schema two ways — the
    /// [`with_output_schema`] combinator on a single op, and [`PluginBuilder::map_operations`] for
    /// bulk metadata sourced separately from handler registration — and both land in the manifest.
    #[test]
    fn output_schema_via_combinator_and_map_operations() {
        let out = json!({ "type": "object", "properties": { "id": { "type": "string" } } });
        let plugin = PluginBuilder::new("acme", "0.1.0")
            .operation_flexible(
                with_output_schema(
                    read_op("acme.get", "get a thing", json!({ "type": "object" })),
                    out.clone(),
                ),
                |_input, _host| Ok(json!({ "id": "1" })),
            )
            .operation_flexible(
                read_op("acme.list", "list things", json!({ "type": "object" })),
                |_input, _host| Ok(json!([])),
            )
            // Bulk-annotate every op declared so far (here: stamp a group), leaving the
            // combinator-set output schema untouched.
            .map_operations(|op| op.group = Some("acme.core".into()))
            .build();

        let m = plugin.manifest();
        let get = m.operations.iter().find(|o| o.name == "acme.get").unwrap();
        assert_eq!(get.output_schema.as_ref(), Some(&out));
        assert_eq!(get.group.as_deref(), Some("acme.core"));
        // The op without an explicit schema keeps `None`; the bulk map still reached it.
        let list = m.operations.iter().find(|o| o.name == "acme.list").unwrap();
        assert!(list.output_schema.is_none());
        assert_eq!(list.group.as_deref(), Some("acme.core"));
    }

    #[test]
    fn ref_based_http_and_credential_helpers() {
        // http_ref resolves the endpoint_ref + path host-side; the plugin never holds a URL. The
        // canned http entry matches by the composed-URL substring, exactly like the real host.
        let mut backend = MockHost::default()
            .with_endpoint_ref("@endpoint/svc-1", "https://svc.internal/v1/")
            .with_http("/v1/ping", json!({ "pong": true }))
            .with_credential("kubernetes/ns/sec/password", "pw-from-cred-ref")
            .with_credential("@endpoint/pg-1", "pw-from-endpoint-ref");
        let mut host = Host {
            inner: &mut backend,
        };
        let v = host.get_json_ref("@endpoint/svc-1", "ping", None).unwrap();
        assert_eq!(v["pong"], true);
        // An unconfigured ref is a clear error (the plugin can't reach an unknown endpoint).
        assert!(host
            .http_ref("@endpoint/nope", "GET", "x", None, &[], None)
            .is_err());
        // The gated `credential` capability materializes by credential_ref or endpoint_ref.
        assert_eq!(
            host.credential("kubernetes/ns/sec/password").unwrap(),
            "pw-from-cred-ref"
        );
        assert_eq!(
            host.credential_for_endpoint("@endpoint/pg-1").unwrap(),
            "pw-from-endpoint-ref"
        );
        assert!(host.credential("kubernetes/ns/sec/missing").is_err());
    }

    #[test]
    fn conn_dial_ref_resolves_and_round_trips() {
        let mut backend = MockHost::default()
            .with_endpoint_ref("@endpoint/db-1", "postgres://db.internal:5432/app")
            .with_conn_response(b"OK".to_vec());
        let mut host = Host {
            inner: &mut backend,
        };
        let id = host.conn_dial_ref("@endpoint/db-1").unwrap();
        assert_eq!(id, 1);
        assert_eq!(host.conn_read(id, 64).unwrap(), b"OK");
        host.conn_close(id).unwrap();
        // Dialing an unconfigured ref errors (the ref drives the target, not global state).
        let mut empty = MockHost::default();
        let mut host2 = Host { inner: &mut empty };
        assert!(host2.conn_dial_ref("@endpoint/unknown").is_err());
    }

    #[test]
    fn conn_methods_round_trip_through_host() {
        let mut backend = MockHost::default();
        let mut host = Host {
            inner: &mut backend,
        };
        let id = host
            .conn_dial(ConnTarget::Tcp {
                host: "db",
                port: 5432,
            })
            .unwrap();
        assert_eq!(id, 1);
        assert_eq!(host.conn_write(id, b"SELECT 1").unwrap(), 8);
        assert_eq!(host.conn_read(id, 64).unwrap(), b"SELECT 1");
        host.conn_close(id).unwrap();
    }

    #[test]
    fn blob_methods_round_trip_through_host() {
        let mut backend = MockHost::default();
        let mut host = Host {
            inner: &mut backend,
        };
        let r = host.blob_put("greeting.txt", b"hi there").unwrap();
        let info = host.blob_info(&r).unwrap();
        assert_eq!(info.name, "greeting.txt");
        assert_eq!(info.size, 8);
        assert_eq!(host.blob_get(&r).unwrap(), b"hi there");
    }

    #[test]
    fn process_methods_round_trip_through_host() {
        let mut backend =
            MockHost::default()
                .with_spawn(7)
                .with_proc_output("forwarding 8080", "", true);
        let mut host = Host {
            inner: &mut backend,
        };
        // spawn returns the canned proc_id (with env overrides accepted)
        let id = host
            .process_spawn(
                &["kubectl", "port-forward", "svc/x", "8080:80"],
                &[("KUBECONFIG", "/k")],
            )
            .unwrap();
        assert_eq!(id, 7);
        // read drains canned output + liveness
        let r = host.process_read(id).unwrap();
        assert_eq!(r.stdout, "forwarding 8080");
        assert!(r.running);
        assert_eq!(r.exit_code, None);
        // status reports liveness
        let st = host.process_status(id).unwrap();
        assert!(st.running);
        // kill is accepted
        host.process_kill(id).unwrap();
    }

    /// D-32: `http_bytes_ref` is the ref-based mirror of `http_bytes` — byte-exact upload/download
    /// against an endpoint **reference**, the plugin never holding a URL. The mock resolves the ref
    /// + joins the path exactly like the real host, so canned entries match by composed URL.
    #[test]
    fn http_bytes_ref_round_trips_binary_by_reference() {
        let raw: Vec<u8> = vec![0, 159, 146, 150, 255];
        let mut backend = MockHost::default()
            .with_endpoint_ref("svc.endpoint", "https://svc.test/api")
            .with_http_bytes("/api/download", raw.clone())
            .with_http("/api/upload", json!({ "ok": true }));
        let mut host = Host {
            inner: &mut backend,
        };
        // binary_response=true → byte-exact download through the ref (non-UTF-8 preserved).
        let dl = host
            .http_bytes_ref("svc.endpoint", "GET", "download", None, &[], None, true)
            .unwrap();
        assert_eq!(dl.status, 200);
        assert_eq!(dl.bytes, raw);
        // Byte-exact upload with headers through the ref; text response bytes come back.
        let up = host
            .http_bytes_ref(
                "svc.endpoint",
                "POST",
                "upload",
                None,
                &[("content-type", "multipart/form-data; boundary=x")],
                Some(b"payload"),
                false,
            )
            .unwrap();
        assert_eq!(up.status, 200);
        assert_eq!(up.bytes, b"{\"ok\":true}");
        // An unconfigured ref is a clear error.
        assert!(host
            .http_bytes_ref("nope.endpoint", "GET", "x", None, &[], None, true)
            .is_err());
    }

    /// D-32: the gated `config` capability reads a declared non-secret config value by name — the
    /// replacement for the config reads that abused the retired `endpoint` URL-handback.
    #[test]
    fn config_reads_declared_value_and_errors_on_unknown() {
        let mut backend = MockHost::default().with_config("cloud_id", "cloud-123");
        let mut host = Host {
            inner: &mut backend,
        };
        assert_eq!(host.config("cloud_id").unwrap(), "cloud-123");
        assert!(host.config("nope").is_err());
    }

    #[test]
    fn http_bytes_round_trips_binary_and_text() {
        let raw: Vec<u8> = vec![0, 159, 146, 150, 255];
        let mut backend = MockHost::default()
            .with_http_bytes("/download", raw.clone())
            .with_http("/upload", json!({ "ok": true }));
        let mut host = Host {
            inner: &mut backend,
        };
        // binary_response=true → byte-exact download (non-UTF-8 preserved)
        let dl = host
            .http_bytes("GET", "https://api.test/download", None, &[], None, true)
            .unwrap();
        assert_eq!(dl.status, 200);
        assert_eq!(dl.bytes, raw);
        // binary_response=false → response bytes are the (text) body's bytes; body upload works too
        let up = host
            .http_bytes(
                "POST",
                "https://api.test/upload",
                None,
                &[],
                Some(b"payload"),
                false,
            )
            .unwrap();
        assert_eq!(up.status, 200);
        // the mock echoes the canned JSON as the text body, whose bytes we get back
        assert_eq!(up.bytes, b"{\"ok\":true}");
    }

    #[test]
    fn operation_helpers_default_to_conservative_staging_inference() {
        let schema = json!({"type": "object"});
        let ops = [
            read_op("acme.read", "read", schema.clone()),
            write_op("acme.write", "write", schema.clone()),
            internal_op("acme.internal", "internal", schema),
        ];

        assert!(ops.iter().all(|op| op.staging == StagingDisposition::Infer));
    }

    /// C-09a: `internal_op`/`internal_op_typed` build an op with `internal: true`, and the host's
    /// `visible_ops` filter excludes it from the projected tool catalog — the model never sees an
    /// `auth` op that returns raw credentials. The op is still in the manifest (host-dispatchable).
    #[test]
    fn internal_op_is_host_only_and_excluded_from_visible_tools() {
        let typed = internal_op_typed::<serde_json::Value>("aws-bedrock.auth", "resolve creds");
        assert!(typed.internal, "internal_op_typed sets internal: true");
        let plain = internal_op(
            "aws-bedrock.auth",
            "resolve creds",
            json!({"type":"object"}),
        );
        assert!(plain.internal, "internal_op sets internal: true");

        // A manifest carrying one public + one internal op projects only the public one.
        let manifest = PluginBuilder::new("aws-bedrock", "0.1.0")
            .operation_flexible(
                read_op("aws-bedrock.chat", "run a turn", json!({})),
                |_, _| Ok(json!({"ok": true})),
            )
            .operation_flexible(
                internal_op("aws-bedrock.auth", "resolve creds", json!({})),
                |_, _| Ok(json!({"access_key": "AKID"})),
            )
            .build()
            .manifest;
        let visible: Vec<&str> = flux_plugin_protocol::visible_ops(&manifest)
            .map(|o| o.name.as_str())
            .collect();
        assert_eq!(visible, vec!["aws-bedrock.chat"]);
        // The internal op is still in the manifest (host-dispatchable), just not projected.
        assert!(manifest
            .operations
            .iter()
            .any(|o| o.name == "aws-bedrock.auth"));
    }

    /// D-88: `build()` auto-registers the reserved `plugin.validate` internal op — present in the
    /// manifest for the host's `--dry-run` path to feature-detect, but never projected as a tool.
    #[test]
    fn validate_op_is_auto_registered_and_internal() {
        let plugin = PluginBuilder::new("acme", "0.1.0")
            .operation_flexible(read_op("acme.ping", "ping", json!({})), |_, _| {
                Ok(json!({"ok": true}))
            })
            .build();
        let m = plugin.manifest();
        let spec = m
            .operations
            .iter()
            .find(|o| o.name == VALIDATE_OP)
            .expect("validate op registered");
        assert!(spec.internal, "validate op is host-only");
        let visible: Vec<&str> = flux_plugin_protocol::visible_ops(&m)
            .map(|o| o.name.as_str())
            .collect();
        assert_eq!(visible, vec!["acme.ping"]);
    }

    /// D-88 keystone: runtime dispatch and the `plugin.validate` answer share one preflight, so a
    /// dry-run verdict and a live call can never disagree.
    #[test]
    fn dispatch_and_validate_op_share_the_preflight_verdict() {
        let schema = json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "state": { "type": ["string", "null"], "enum": ["open", "closed", null] },
            },
            "required": ["name"],
            "additionalProperties": false,
        });
        let plugin = PluginBuilder::new("acme", "0.1.0")
            .operation_flexible(read_op("acme.thing", "fetch", schema), |_, _| {
                Ok(json!({"fetched": true}))
            })
            // A custom rule the schema can't express: `name` may not be "root".
            .preflight("acme.thing", |input| {
                match input.get("name").and_then(|v| v.as_str()) {
                    Some("root") => vec!["`name`: \"root\" is reserved".into()],
                    _ => Vec::new(),
                }
            })
            .build();
        let mut host = MockHost::default();

        // A conforming input dispatches to the handler.
        let ok = plugin
            .call(
                "acme.thing",
                json!({"name": "x", "state": "open"}),
                &mut host,
            )
            .expect("valid input runs");
        assert_eq!(ok["fetched"], true);

        // Schema problems (blank required, bad enum, unknown field) block dispatch...
        let err = plugin
            .call(
                "acme.thing",
                json!({"name": "  ", "state": "weird", "typo": 1}),
                &mut host,
            )
            .unwrap_err();
        assert!(err.contains("invalid input for `acme.thing`"), "{err}");
        assert!(err.contains("blank"), "{err}");
        // ...and the validate op reports the SAME problems without executing anything.
        let verdict = plugin
            .call(
                VALIDATE_OP,
                json!({"operation": "acme.thing", "input": {"name": "  ", "state": "weird", "typo": 1}}),
                &mut host,
            )
            .expect("validate answers");
        assert_eq!(verdict["valid"], false);
        assert_eq!(verdict["problems"].as_array().unwrap().len(), 3);

        // The custom rule fires in both paths too.
        let err = plugin
            .call("acme.thing", json!({"name": "root"}), &mut host)
            .unwrap_err();
        assert!(err.contains("reserved"), "{err}");
        let verdict = plugin
            .call(
                VALIDATE_OP,
                json!({"operation": "acme.thing", "input": {"name": "root"}}),
                &mut host,
            )
            .unwrap();
        assert_eq!(verdict["valid"], false);

        // Validating an unknown op is an error (mirrors dispatch), not a verdict.
        assert!(plugin
            .call(VALIDATE_OP, json!({"operation": "acme.nope"}), &mut host)
            .is_err());
        // Omitted `input` validates as an empty object.
        let verdict = plugin
            .call(VALIDATE_OP, json!({"operation": "acme.thing"}), &mut host)
            .unwrap();
        assert_eq!(verdict["valid"], false, "missing required `name`");
    }

    #[test]
    #[should_panic(expected = "preflight rule for unregistered op")]
    fn preflight_rule_for_unregistered_op_panics_at_build() {
        let _ = PluginBuilder::new("acme", "0.1.0")
            .preflight("acme.nope", |_| Vec::new())
            .build();
    }
}
