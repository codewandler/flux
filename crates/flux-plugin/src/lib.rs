//! `flux-plugin` — the subprocess plugin protocol, host, and SDK.
//!
//! Plugins are native binaries in any language that speak a line-delimited JSON protocol over
//! stdio: the host writes one [`Frame`] (request) per line to the plugin's stdin and reads one
//! [`Frame`] (response) per line from its stdout. Plugins never do their own privileged IO — in
//! the full design every side effect is a host-capability call back over the same channel
//! (HTTP/process/blob/…); v1 implements `manifest` + `operation.call`. WASM can later be an
//! alternate transport behind the same protocol/manifest.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use flux_core::{Error, Result};
use flux_runtime::{Tool, ToolContext, ToolResult};
use flux_spec::{Idempotency, Risk, ToolSpec};

pub const PROTOCOL: &str = "flux.plugin.v1";

/// Whether a frame is a request (host→plugin) or a response (plugin→host).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FrameKind {
    Request,
    Response,
}

/// One protocol message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    pub protocol: String,
    pub id: String,
    #[serde(rename = "type")]
    pub kind: FrameKind,
    pub command: String,
    #[serde(default)]
    pub payload: Value,
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub result: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Frame {
    pub fn request(id: impl Into<String>, command: impl Into<String>, payload: Value) -> Self {
        Self {
            protocol: PROTOCOL.into(),
            id: id.into(),
            kind: FrameKind::Request,
            command: command.into(),
            payload,
            ok: false,
            result: Value::Null,
            error: None,
        }
    }

    pub fn ok_response(id: &str, result: Value) -> Self {
        Self {
            protocol: PROTOCOL.into(),
            id: id.into(),
            kind: FrameKind::Response,
            command: String::new(),
            payload: Value::Null,
            ok: true,
            result,
            error: None,
        }
    }

    pub fn err_response(id: &str, error: impl Into<String>) -> Self {
        Self {
            protocol: PROTOCOL.into(),
            id: id.into(),
            kind: FrameKind::Response,
            command: String::new(),
            payload: Value::Null,
            ok: false,
            result: Value::Null,
            error: Some(error.into()),
        }
    }
}

/// A plugin-declared operation (becomes a tool projected to the agent, after the policy gate).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationSpec {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub input_schema: Value,
}

/// What a plugin advertises about itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub operations: Vec<OperationSpec>,
}

// ---------------------------------------------------------------------------
// Plugin SDK (guest side) — synchronous stdio loop
// ---------------------------------------------------------------------------

/// A handle a plugin uses to call back into the host (host capabilities) during an operation.
/// Each call writes a request frame to stdout and blocks for the host's response on stdin.
pub trait GuestHost {
    fn host_call(&mut self, command: &str, payload: Value) -> std::result::Result<Value, String>;
}

/// Implemented by a plugin: advertise a manifest, handle operation calls. The `host` handle lets
/// an operation call back into the host for privileged IO (HTTP/process/secret) — plugins do no
/// privileged IO of their own.
pub trait PluginHandler {
    fn manifest(&self) -> PluginManifest;
    fn call(
        &self,
        operation: &str,
        input: Value,
        host: &mut dyn GuestHost,
    ) -> std::result::Result<Value, String>;
}

/// The concrete [`GuestHost`] used by [`serve`]: writes plugin→host request frames and reads the
/// host's response, sharing the same stdio the serve loop uses (sequentially, never concurrently).
struct StdioGuestHost<'a, R: std::io::BufRead, W: std::io::Write> {
    reader: &'a mut R,
    writer: &'a mut W,
    next: u64,
}

impl<R: std::io::BufRead, W: std::io::Write> GuestHost for StdioGuestHost<'_, R, W> {
    fn host_call(&mut self, command: &str, payload: Value) -> std::result::Result<Value, String> {
        self.next += 1;
        let frame = Frame::request(format!("h{}", self.next), command, payload);
        let mut line = serde_json::to_string(&frame).map_err(|e| e.to_string())?;
        line.push('\n');
        self.writer
            .write_all(line.as_bytes())
            .map_err(|e| e.to_string())?;
        self.writer.flush().map_err(|e| e.to_string())?;

        let mut resp = String::new();
        match self.reader.read_line(&mut resp) {
            Ok(0) => return Err("host closed the connection".into()),
            Ok(_) => {}
            Err(e) => return Err(e.to_string()),
        }
        let frame: Frame = serde_json::from_str(resp.trim()).map_err(|e| e.to_string())?;
        if frame.ok {
            Ok(frame.result)
        } else {
            Err(frame.error.unwrap_or_default())
        }
    }
}

fn write_line<W: std::io::Write>(writer: &mut W, frame: &Frame) {
    if let Ok(mut out) = serde_json::to_string(frame) {
        out.push('\n');
        let _ = writer.write_all(out.as_bytes());
        let _ = writer.flush();
    }
}

/// Run the plugin: read request frames from stdin, dispatch, write response frames to stdout.
/// Operation calls may issue host-capability callbacks via the provided [`GuestHost`]. Blocks
/// until stdin closes. Call this from a plugin binary's `main`.
pub fn serve(handler: impl PluginHandler) {
    use std::io::BufRead;
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = stdin.lock();
    let mut writer = stdout.lock();
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break, // EOF or read error
            Ok(_) => {}
        }
        if line.trim().is_empty() {
            continue;
        }
        let Ok(req) = serde_json::from_str::<Frame>(line.trim()) else {
            continue;
        };
        let resp = match req.command.as_str() {
            "manifest" => match serde_json::to_value(handler.manifest()) {
                Ok(v) => Frame::ok_response(&req.id, v),
                Err(e) => Frame::err_response(&req.id, e.to_string()),
            },
            "operation.call" => {
                let op = req
                    .payload
                    .get("operation")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let input = req.payload.get("input").cloned().unwrap_or(Value::Null);
                let mut host = StdioGuestHost {
                    reader: &mut reader,
                    writer: &mut writer,
                    next: 0,
                };
                match handler.call(op, input, &mut host) {
                    Ok(v) => Frame::ok_response(&req.id, v),
                    Err(e) => Frame::err_response(&req.id, e),
                }
            }
            other => Frame::err_response(&req.id, format!("unknown command: {other}")),
        };
        write_line(&mut writer, &resp);
    }
}

// ---------------------------------------------------------------------------
// Host capabilities (the only IO a plugin gets, all routed through the guarded host)
// ---------------------------------------------------------------------------

/// The privileged operations a plugin may request of the host during an operation call. Every
/// capability is policy-relevant IO the plugin cannot do itself; the host services it through the
/// guarded [`System`](flux_system::System) and returns a result frame.
#[async_trait]
pub trait HostCapabilities: Send + Sync {
    async fn handle(&self, command: &str, payload: &Value) -> std::result::Result<Value, String>;
}

/// Denies every host-capability callback (the default for `call`). A plugin that needs callbacks
/// must be driven via [`PluginHost::call_with_host`] with a real [`HostCapabilities`].
pub struct DenyHostCaps;

#[async_trait]
impl HostCapabilities for DenyHostCaps {
    async fn handle(&self, command: &str, _p: &Value) -> std::result::Result<Value, String> {
        Err(format!("host capability `{command}` is not available"))
    }
}

/// Host capabilities backed by the guarded [`System`](flux_system::System): `process.run` (argv
/// only), `http.do` (GET, loopback/private blocked unless allowed), and `secret` (env refs). This
/// is the bridge that keeps plugin IO inside the same safety boundary as the agent's own tools.
pub struct SystemHostCaps {
    system: Arc<flux_system::System>,
    allow_private_net: bool,
}

impl SystemHostCaps {
    pub fn new(system: Arc<flux_system::System>) -> Self {
        Self {
            system,
            allow_private_net: false,
        }
    }

    pub fn allow_private_net(mut self, yes: bool) -> Self {
        self.allow_private_net = yes;
        self
    }
}

#[async_trait]
impl HostCapabilities for SystemHostCaps {
    async fn handle(&self, command: &str, payload: &Value) -> std::result::Result<Value, String> {
        match command {
            "process.run" => {
                let argv: Vec<String> = payload
                    .get("argv")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                if argv.is_empty() {
                    return Err("process.run: `argv` (non-empty array) required".into());
                }
                let secs = payload
                    .get("timeout_secs")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(60);
                let out = self
                    .system
                    .run(&argv, std::time::Duration::from_secs(secs))
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(
                    json!({ "stdout": out.stdout, "stderr": out.stderr, "exit_code": out.exit_code }),
                )
            }
            "secret" => {
                let key = payload.get("key").and_then(|v| v.as_str()).unwrap_or("");
                match self.system.env(key) {
                    Some(v) => Ok(json!({ "value": v })),
                    None => Err(format!("secret `{key}` not set")),
                }
            }
            "http.do" => {
                let raw = payload.get("url").and_then(|v| v.as_str()).unwrap_or("");
                let url = guard_http_url(raw, self.allow_private_net)?;
                let resp = reqwest::Client::new()
                    .get(url)
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                let status = resp.status().as_u16();
                let mut body = resp.text().await.unwrap_or_default();
                body.truncate(64 * 1024);
                Ok(json!({ "status": status, "body": body }))
            }
            other => Err(format!("unknown host capability: {other}")),
        }
    }
}

/// Reject non-HTTP(S) schemes and (unless `allow_private`) loopback/private/link-local hosts.
fn guard_http_url(raw: &str, allow_private: bool) -> std::result::Result<url::Url, String> {
    let url = url::Url::parse(raw).map_err(|e| format!("invalid url: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!("unsupported url scheme: {}", url.scheme()));
    }
    if allow_private {
        return Ok(url);
    }
    let host = url
        .host_str()
        .ok_or_else(|| "url has no host".to_string())?;
    if host.eq_ignore_ascii_case("localhost") {
        return Err("refusing to fetch localhost".into());
    }
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        let blocked = match ip {
            std::net::IpAddr::V4(v4) => {
                v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified()
            }
            std::net::IpAddr::V6(v6) => v6.is_loopback() || v6.is_unspecified(),
        };
        if blocked {
            return Err(format!("refusing to fetch private/loopback address {ip}"));
        }
    }
    Ok(url)
}

// ---------------------------------------------------------------------------
// Plugin host (host side) — async, spawns the subprocess
// ---------------------------------------------------------------------------

/// A running plugin subprocess the host talks to over framed stdio.
pub struct PluginHost {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    reader: tokio::io::BufReader<tokio::process::ChildStdout>,
    next_id: u64,
}

impl PluginHost {
    /// Spawn a plugin binary.
    pub async fn spawn(program: &str, args: &[String]) -> Result<Self> {
        use std::process::Stdio;
        let mut child = tokio::process::Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| Error::Other(format!("spawn plugin {program}: {e}")))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::Other("plugin stdin unavailable".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Other("plugin stdout unavailable".into()))?;
        Ok(Self {
            child,
            stdin,
            reader: tokio::io::BufReader::new(stdout),
            next_id: 0,
        })
    }

    async fn write_frame(&mut self, frame: &Frame) -> Result<()> {
        use tokio::io::AsyncWriteExt;
        let mut line = serde_json::to_string(frame)?;
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(Error::Io)?;
        self.stdin.flush().await.map_err(Error::Io)?;
        Ok(())
    }

    async fn read_frame(&mut self) -> Result<Frame> {
        use tokio::io::AsyncBufReadExt;
        let mut resp = String::new();
        let n = self.reader.read_line(&mut resp).await.map_err(Error::Io)?;
        if n == 0 {
            return Err(Error::Provider("plugin closed the connection".into()));
        }
        Ok(serde_json::from_str(resp.trim())?)
    }

    async fn request(&mut self, command: &str, payload: Value) -> Result<Frame> {
        self.next_id += 1;
        let frame = Frame::request(format!("r{}", self.next_id), command, payload);
        self.write_frame(&frame).await?;
        self.read_frame().await
    }

    /// Fetch the plugin's manifest.
    pub async fn manifest(&mut self) -> Result<PluginManifest> {
        let f = self.request("manifest", Value::Null).await?;
        if !f.ok {
            return Err(Error::Provider(f.error.unwrap_or_default()));
        }
        Ok(serde_json::from_value(f.result)?)
    }

    /// Call an operation with no host capabilities (callbacks are denied).
    pub async fn call(&mut self, operation: &str, input: Value) -> Result<Value> {
        self.call_with_host(operation, input, &DenyHostCaps).await
    }

    /// Call an operation, servicing any plugin→host capability callbacks via `host` until the
    /// operation's own response arrives. Callbacks and the final response are multiplexed on the
    /// same channel and demultiplexed by frame kind + id.
    pub async fn call_with_host(
        &mut self,
        operation: &str,
        input: Value,
        host: &dyn HostCapabilities,
    ) -> Result<Value> {
        self.next_id += 1;
        let call_id = format!("r{}", self.next_id);
        let frame = Frame::request(
            &call_id,
            "operation.call",
            json!({ "operation": operation, "input": input }),
        );
        self.write_frame(&frame).await?;

        loop {
            let f = self.read_frame().await?;
            match f.kind {
                FrameKind::Request => {
                    // A host-capability callback from the plugin — service it and reply.
                    let reply = match host.handle(&f.command, &f.payload).await {
                        Ok(v) => Frame::ok_response(&f.id, v),
                        Err(e) => Frame::err_response(&f.id, e),
                    };
                    self.write_frame(&reply).await?;
                }
                FrameKind::Response => {
                    if f.id == call_id {
                        return if f.ok {
                            Ok(f.result)
                        } else {
                            Err(Error::Provider(f.error.unwrap_or_default()))
                        };
                    }
                    // A stray/duplicate response — ignore and keep reading.
                }
            }
        }
    }

    /// Terminate the plugin.
    pub async fn shutdown(mut self) -> Result<()> {
        drop(self.stdin); // closing stdin lets a well-behaved plugin exit
        let _ = self.child.kill().await;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// PluginTool — project a plugin operation as an agent tool
// ---------------------------------------------------------------------------

/// Projects a single plugin [`OperationSpec`] as an agent [`Tool`]. All of a plugin's operations
/// share one [`PluginHost`] subprocess behind a mutex; each call goes through the same safety
/// envelope as built-in tools and may issue host-capability callbacks via [`HostCapabilities`].
pub struct PluginTool {
    host: Arc<tokio::sync::Mutex<PluginHost>>,
    caps: Arc<dyn HostCapabilities>,
    plugin: String,
    operation: String,
    spec: ToolSpec,
}

impl PluginTool {
    pub fn new(
        host: Arc<tokio::sync::Mutex<PluginHost>>,
        caps: Arc<dyn HostCapabilities>,
        plugin: &str,
        op: &OperationSpec,
    ) -> Self {
        // Plugin operations declare no effects, so the policy layer doesn't auto-allow them — the
        // perm rules + approval gate apply (they prompt by default under their `plugin.op` name).
        let spec = ToolSpec {
            name: format!("{plugin}.{}", op.name),
            description: op.description.clone(),
            input_schema: op.input_schema.clone(),
            output_schema: None,
            effects: Vec::new(),
            risk: Risk::Medium,
            idempotency: Idempotency::NonIdempotent,
            access: Vec::new(),
        };
        Self {
            host,
            caps,
            plugin: plugin.to_string(),
            operation: op.name.clone(),
            spec,
        }
    }
}

#[async_trait]
impl Tool for PluginTool {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    fn permission_subjects(&self, _params: &Value) -> Vec<String> {
        vec![format!("{}.{}", self.plugin, self.operation)]
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let mut host = self.host.lock().await;
        match host
            .call_with_host(&self.operation, params, self.caps.as_ref())
            .await
        {
            Ok(v) => Ok(ToolResult::ok(
                serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string()),
            )),
            Err(e) => Ok(ToolResult::error(e.to_string())),
        }
    }
}

/// Spawn a plugin, fetch its manifest, and project every operation as a [`PluginTool`] sharing one
/// host connection. Returns the tools plus the shared host handle (keep it alive for the session).
pub async fn load_plugin_tools(
    program: &str,
    args: &[String],
    caps: Arc<dyn HostCapabilities>,
) -> Result<(Vec<Arc<dyn Tool>>, Arc<tokio::sync::Mutex<PluginHost>>)> {
    let mut host = PluginHost::spawn(program, args).await?;
    let manifest = host.manifest().await?;
    let host = Arc::new(tokio::sync::Mutex::new(host));
    let tools: Vec<Arc<dyn Tool>> = manifest
        .operations
        .iter()
        .map(|op| {
            Arc::new(PluginTool::new(
                host.clone(),
                caps.clone(),
                &manifest.name,
                op,
            )) as Arc<dyn Tool>
        })
        .collect();
    Ok((tools, host))
}

// ---------------------------------------------------------------------------
// Discovery & lifecycle — descriptors under ~/.flux/plugins/<name>.toml
// ---------------------------------------------------------------------------

/// A persisted plugin descriptor (`~/.flux/plugins/<name>.toml`): how to launch the plugin plus an
/// optional pinned version. `flux plugin add|ls|pin|rollback` manage these; discovery loads them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginDescriptor {
    /// The plugin executable (absolute path or a name on `PATH`).
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// The pinned version, if any (advisory; surfaced by `flux plugin ls`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<String>,
}

/// A discovered plugin: its name (the descriptor file stem) and how to launch it.
#[derive(Debug, Clone)]
pub struct DiscoveredPlugin {
    pub name: String,
    pub descriptor: PluginDescriptor,
}

fn descriptor_path(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    dir.join(format!("{name}.toml"))
}

/// Read every `<dir>/*.toml` plugin descriptor (sorted by name). Missing dir → empty; malformed
/// descriptors are skipped (never fail discovery for one bad file).
pub fn discover(dir: &std::path::Path) -> Vec<DiscoveredPlugin> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|s| s.to_str()).map(String::from) else {
            continue;
        };
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(descriptor) = toml::from_str::<PluginDescriptor>(&text) {
                out.push(DiscoveredPlugin { name, descriptor });
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Write a plugin descriptor to `<dir>/<name>.toml`, creating `dir` if needed.
pub fn add_descriptor(
    dir: &std::path::Path,
    name: &str,
    descriptor: &PluginDescriptor,
) -> Result<()> {
    std::fs::create_dir_all(dir).map_err(Error::Io)?;
    let body = toml::to_string_pretty(descriptor)
        .map_err(|e| Error::Other(format!("serialize descriptor: {e}")))?;
    std::fs::write(descriptor_path(dir, name), body).map_err(Error::Io)?;
    Ok(())
}

/// Load a single named descriptor, if present.
pub fn load_descriptor(dir: &std::path::Path, name: &str) -> Result<Option<PluginDescriptor>> {
    match std::fs::read_to_string(descriptor_path(dir, name)) {
        Ok(text) => {
            Ok(Some(toml::from_str(&text).map_err(|e| {
                Error::Other(format!("parse descriptor: {e}"))
            })?))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::Io(e)),
    }
}

/// Set or clear the pinned version of a plugin (`flux plugin pin` / `rollback`).
pub fn set_pinned(dir: &std::path::Path, name: &str, version: Option<String>) -> Result<()> {
    let mut d = load_descriptor(dir, name)?
        .ok_or_else(|| Error::Other(format!("no such plugin: {name}")))?;
    d.pinned = version;
    add_descriptor(dir, name, &d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrips_as_ndjson() {
        let f = Frame::request("r1", "manifest", Value::Null);
        let line = serde_json::to_string(&f).unwrap();
        assert!(!line.contains('\n'));
        let back: Frame = serde_json::from_str(&line).unwrap();
        assert_eq!(back.command, "manifest");
        assert_eq!(back.kind, FrameKind::Request);
    }

    #[test]
    fn responses_carry_ok_and_error() {
        let ok = Frame::ok_response("r1", serde_json::json!({"x": 1}));
        assert!(ok.ok);
        assert_eq!(ok.result["x"], 1);
        let err = Frame::err_response("r1", "boom");
        assert!(!err.ok);
        assert_eq!(err.error.as_deref(), Some("boom"));
    }

    #[test]
    fn descriptors_add_discover_pin_rollback() {
        let dir = std::env::temp_dir().join(format!("flux-plugins-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        // missing dir → empty discovery
        assert!(discover(&dir).is_empty());

        add_descriptor(
            &dir,
            "gitlab",
            &PluginDescriptor {
                program: "/usr/bin/gitlab-plugin".into(),
                args: vec!["--v2".into()],
                pinned: None,
            },
        )
        .unwrap();
        add_descriptor(
            &dir,
            "slack",
            &PluginDescriptor {
                program: "slack-plugin".into(),
                args: vec![],
                pinned: None,
            },
        )
        .unwrap();

        let found = discover(&dir);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].name, "gitlab"); // sorted
        assert_eq!(found[0].descriptor.args, vec!["--v2"]);

        set_pinned(&dir, "gitlab", Some("1.2.3".into())).unwrap();
        assert_eq!(
            load_descriptor(&dir, "gitlab")
                .unwrap()
                .unwrap()
                .pinned
                .as_deref(),
            Some("1.2.3")
        );
        set_pinned(&dir, "gitlab", None).unwrap(); // rollback clears the pin
        assert!(load_descriptor(&dir, "gitlab")
            .unwrap()
            .unwrap()
            .pinned
            .is_none());

        std::fs::remove_dir_all(&dir).ok();
    }
}
