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
        let v = self
            .inner
            .host_call("conn.read", json!({ "conn_id": conn_id, "max": max }))?;
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
    }
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
    /// `(argv-substring) -> stdout string for process.run` (matched in insertion order).
    pub process: Vec<(String, String)>,
    /// Records the plugin contributed (captured for assertions).
    pub contributed: std::cell::RefCell<Vec<Record>>,
    /// An in-memory `conn.*` byte buffer: `conn.write` appends, `conn.read` drains (a loopback echo).
    pub conn_buf: std::cell::RefCell<Vec<u8>>,
    /// An in-memory `blob.*` store: `blob_ref -> (name, bytes)`.
    pub blobs: std::cell::RefCell<HashMap<String, (String, Vec<u8>)>>,
}

impl Default for MockHost {
    fn default() -> Self {
        Self {
            http: Vec::new(),
            secrets: HashMap::new(),
            endpoints: HashMap::new(),
            process: Vec::new(),
            contributed: std::cell::RefCell::new(Vec::new()),
            conn_buf: std::cell::RefCell::new(Vec::new()),
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
    /// A resolvable endpoint base URL.
    pub fn with_endpoint(mut self, name: &str, url: &str) -> Self {
        self.endpoints.insert(name.into(), url.into());
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
                let url = payload.get("url").and_then(|v| v.as_str()).unwrap_or("");
                let body = self
                    .http
                    .iter()
                    .find(|(sub, _)| url.contains(sub.as_str()))
                    .map(|(_, v)| v.clone())
                    .ok_or_else(|| format!("mock: no canned http for `{url}`"))?;
                Ok(json!({ "status": 200, "body": serde_json::to_string(&body).unwrap() }))
            }
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
            "conn.dial" => Ok(json!({ "conn_id": 1 })),
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
                let mut buf = self.conn_buf.borrow_mut();
                let take = buf.len().min(max);
                let out: Vec<u8> = buf.drain(..take).collect();
                Ok(json!({
                    "data_b64": base64::engine::general_purpose::STANDARD.encode(&out),
                    "eof": out.is_empty()
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
}
