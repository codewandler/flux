//! `host-kit` — the shared SDK for flux integration plugins (story D-08).
//!
//! It wraps flux-plugin's guest protocol so a plugin is mostly "declare ops + implement each against a
//! vendor API": a typed [`Host`] for the host-capability callbacks (secret-by-purpose, HTTP with
//! auth-by-scheme injection, endpoint resolution, datasource-record contribution) and a [`PluginBuilder`] that collects
//! a manifest + op handlers and serves them. Plugins never read state files or hold raw tokens for the
//! auth-injection path — the host resolves secrets and injects them.
//!
//! ```ignore
//! use host_kit::*;
//! fn main() {
//!     PluginBuilder::new("acme", "0.1.0")
//!         .capabilities(Caps { http: true, secrets: vec!["ACME_TOKEN".into()], ..Caps::default() })
//!         .auth(AuthMethod { purpose: "api_token".into(), env: vec!["ACME_TOKEN".into()], ..Default::default() })
//!         .endpoint(EndpointSpec { name: "acme.endpoint".into(), env: vec!["ACME_URL".into()], ..Default::default() })
//!         .operation(op("acme.ping", "Ping the API", schema), |_in, host| {
//!             let base = host.endpoint("acme.endpoint")?;
//!             let v = host.get_json(&format!("{base}/ping"), Some("api_token"))?;
//!             Ok(v)
//!         })
//!         .serve();
//! }
//! ```

use std::collections::HashMap;

use base64::Engine as _;
use serde_json::{json, Value};

// Re-export the protocol vocabulary so a plugin depends only on host-kit.
pub use flux_datasource::{Declaration, EntitySchema, Link, Record, SchemaField, Source};
pub use flux_plugin::{
    AuthMethod, AuthScheme, EndpointSpec, GuestHost, OperationSpec, PluginCapabilities as Caps,
    PluginHandler, PluginManifest,
};
pub use flux_spec::{Effect, Idempotency, Risk};

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
pub fn op_input_schema<T: schemars::JsonSchema>() -> Value {
    flux_spec::tool_input_schema::<T>()
}

/// A typed view over the host-capability channel, handed to each op handler.
pub struct Host<'a> {
    inner: &'a mut dyn GuestHost,
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

    /// Resolve a named endpoint base URL (config, from env).
    pub fn endpoint(&mut self, name: &str) -> Result<String, String> {
        let v = self.inner.host_call("endpoint", json!({ "name": name }))?;
        v.get("url")
            .and_then(|x| x.as_str())
            .map(String::from)
            .ok_or_else(|| "endpoint: host returned no url".into())
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
    /// injects per its declared scheme — same as [`http`](Self::http), but the URL stays host-only.
    pub fn http_ref(
        &mut self,
        endpoint_ref: &str,
        method: &str,
        path: &str,
        auth_purpose: Option<&str>,
        body: Option<&[u8]>,
    ) -> Result<HttpResponse, String> {
        let mut payload = json!({ "method": method, "endpoint_ref": endpoint_ref, "path": path });
        if let Some(p) = auth_purpose {
            payload["auth_purpose"] = json!(p);
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

    /// Convenience: GET an endpoint-reference path (optional auth purpose) and parse the JSON body,
    /// erroring on non-2xx. The ref-based mirror of [`get_json`](Self::get_json).
    pub fn get_json_ref(
        &mut self,
        endpoint_ref: &str,
        path: &str,
        auth_purpose: Option<&str>,
    ) -> Result<Value, String> {
        let resp = self.http_ref(endpoint_ref, "GET", path, auth_purpose, None)?;
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
        let resp = self.http_ref(endpoint_ref, method, path, auth_purpose, Some(s.as_bytes()))?;
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
    /// wire protocol (SQL, AMI, the Docker socket) the host never speaks itself.
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
    /// plugin (SQL, AMI) reaches a discovered endpoint without ever holding a URL.
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
    /// wire-protocol loop can distinguish a deadline from a clean EOF (D-45: sql/asterisk `timeout`).
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
/// Asterisk AMI line protocol, HTTP/1.1 over the Docker unix socket — on top of standard buffered IO
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

/// A handler closure for one operation: `(input, host) -> result`.
type OpFn = Box<dyn Fn(Value, &mut Host) -> Result<Value, String> + Send + Sync>;

/// Collects a manifest + op handlers, then [`serve`](Plugin::serve)s them over the plugin protocol.
pub struct PluginBuilder {
    manifest: PluginManifest,
    ops: HashMap<String, OpFn>,
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
        }
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

    /// Add a configurable endpoint (base URL from env).
    pub fn endpoint(mut self, ep: EndpointSpec) -> Self {
        self.manifest.endpoints.push(ep);
        self
    }

    /// Declare a datasource this plugin contributes records for.
    pub fn datasource(mut self, decl: Declaration) -> Self {
        self.manifest.datasources.push(decl);
        self
    }

    /// Declare a product this plugin can **discover** endpoints for as a provider (D-26). The host's
    /// fan-out broker routes a consumer's discovery query for this product to this plugin's
    /// `endpoint.discover` op. Call once per product.
    pub fn discovers(mut self, product: impl Into<String>) -> Self {
        self.manifest.discovers.push(product.into());
        self
    }

    /// Register an operation: its spec (projected to a tool) + the handler closure.
    pub fn operation(
        mut self,
        spec: OperationSpec,
        handler: impl Fn(Value, &mut Host) -> Result<Value, String> + Send + Sync + 'static,
    ) -> Self {
        self.ops.insert(spec.name.clone(), Box::new(handler));
        self.manifest.operations.push(spec);
        self
    }

    /// Finish building (without serving) — used by tests to call ops against a mock host.
    pub fn build(self) -> Plugin {
        Plugin {
            manifest: self.manifest,
            ops: self.ops,
        }
    }

    /// Build and run the stdio serve loop (call from `main`).
    pub fn serve(self) {
        flux_plugin::serve(self.build());
    }
}

/// A built plugin: a [`PluginHandler`] dispatching to the registered op closures.
pub struct Plugin {
    manifest: PluginManifest,
    ops: HashMap<String, OpFn>,
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
        let op = self
            .ops
            .get(operation)
            .ok_or_else(|| format!("unknown operation: {operation}"))?;
        let mut h = Host { inner: host };
        op(input, &mut h)
    }
}

/// A simple read-only operation spec helper (Effect::Read, low risk, idempotent).
pub fn read_op(name: &str, description: &str, input_schema: Value) -> OperationSpec {
    OperationSpec {
        name: name.into(),
        description: description.into(),
        input_schema,
        effects: vec![Effect::Read],
        risk: Some(Risk::Low),
        idempotency: Some(Idempotency::Idempotent),
        secret_purposes: Vec::new(),
        internal: false,
    }
}

/// A write/mutating operation spec helper (Effect::Write, medium risk, non-idempotent).
pub fn write_op(name: &str, description: &str, input_schema: Value) -> OperationSpec {
    OperationSpec {
        name: name.into(),
        description: description.into(),
        input_schema,
        effects: vec![Effect::Write, Effect::Network],
        risk: Some(Risk::Medium),
        idempotency: Some(Idempotency::NonIdempotent),
        secret_purposes: Vec::new(),
        internal: false,
    }
}

/// A **typed** read-only op: `input_schema` is derived from `T` via `schemars`
/// ([`op_input_schema`]) instead of a hand-written `json!({...})` object.
///
/// `T` should be a `#[derive(Deserialize, schemars::JsonSchema)]` struct whose fields encode
/// the op's params (use `Option<T>` for optional fields so `required` is a set, per L-09).
/// Add `#[schemars(allow_unknown_fields)]` when the handler ignores unknown keys (the common
/// case for flex-extractors) so the derived schema doesn't forbid extras the runtime accepts.
/// Effects/risk/idempotency match [`read_op`] (Read, Low, Idempotent).
pub fn read_op_typed<T: schemars::JsonSchema>(name: &str, description: &str) -> OperationSpec {
    read_op(name, description, op_input_schema::<T>())
}

/// A **typed** write/mutating op: `input_schema` derived from `T` via `schemars`
/// ([`op_input_schema`]). Effects/risk/idempotency match [`write_op`] (Write+Network,
/// Medium, NonIdempotent). See [`read_op_typed`] for the `T` contract.
pub fn write_op_typed<T: schemars::JsonSchema>(name: &str, description: &str) -> OperationSpec {
    write_op(name, description, op_input_schema::<T>())
}

/// A **host-only** op (C-09a): not advertised to the LLM as a callable tool. The canonical case is
/// the `aws-bedrock` plugin's `auth` op, which returns raw AWS credentials — the model must never
/// call it, or the keys would appear in the tool result. The op stays dispatchable by the host via
/// the shared `PluginHost` handle; [`flux_plugin::visible_ops`] excludes it from the projected tool
/// catalog. Effects default to `Process`+`Network` (the conservative authorization floor
/// [`flux_plugin::PluginTool::new`] applies to an undeclared op) — override via the returned spec.
pub fn internal_op(name: &str, description: &str, input_schema: Value) -> OperationSpec {
    OperationSpec {
        name: name.into(),
        description: description.into(),
        input_schema,
        effects: Vec::new(),
        risk: Some(Risk::Low),
        idempotency: Some(Idempotency::Idempotent),
        secret_purposes: Vec::new(),
        internal: true,
    }
}

/// A **typed** host-only op: `input_schema` derived from `T` via `schemars` ([`op_input_schema`]).
/// See [`internal_op`] for the host-only contract.
pub fn internal_op_typed<T: schemars::JsonSchema>(name: &str, description: &str) -> OperationSpec {
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
    /// `endpoint name -> base url`.
    pub endpoints: HashMap<String, String>,
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
    /// hand-rolled wire-protocol client (SQL/AMI/Docker) can be tested without a real socket.
    pub conn_script: std::cell::RefCell<std::collections::VecDeque<Vec<u8>>>,
    /// An in-memory `blob.*` store: `blob_ref -> (name, bytes)`.
    pub blobs: std::cell::RefCell<HashMap<String, (String, Vec<u8>)>>,
}

impl Default for MockHost {
    fn default() -> Self {
        Self {
            http: Vec::new(),
            secrets: HashMap::new(),
            endpoints: HashMap::new(),
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
    /// A resolvable endpoint base URL.
    pub fn with_endpoint(mut self, name: &str, url: &str) -> Self {
        self.endpoints.insert(name.into(), url.into());
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
        match command {
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
            "endpoint" => {
                let n = payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
                self.endpoints
                    .get(n)
                    .map(|u| json!({ "url": u }))
                    .ok_or_else(|| format!("mock: no endpoint `{n}`"))
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
                description: String::new(),
            })
            .operation(
                read_op("acme.thing", "fetch a thing", json!({"type": "object"})),
                |_input, host| {
                    let base = host.endpoint("acme.endpoint")?;
                    let v = host.get_json(&format!("{base}/things/1"), Some("api_token"))?;
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

        // manifest carries the op + auth + endpoint
        let m = plugin.manifest();
        assert_eq!(m.operations.len(), 1);
        assert_eq!(m.auth[0].purpose, "api_token");

        let mut host = MockHost::default()
            .with_endpoint("acme.endpoint", "https://acme.test")
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
            .http_ref("@endpoint/nope", "GET", "x", None, None)
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
            .operation(
                read_op("aws-bedrock.chat", "run a turn", json!({})),
                |_, _| Ok(json!({"ok": true})),
            )
            .operation(
                internal_op("aws-bedrock.auth", "resolve creds", json!({})),
                |_, _| Ok(json!({"access_key": "AKID"})),
            )
            .build()
            .manifest;
        let visible: Vec<&str> = flux_plugin::visible_ops(&manifest)
            .map(|o| o.name.as_str())
            .collect();
        assert_eq!(visible, vec!["aws-bedrock.chat"]);
        // The internal op is still in the manifest (host-dispatchable), just not projected.
        assert!(manifest
            .operations
            .iter()
            .any(|o| o.name == "aws-bedrock.auth"));
    }
}
