//! `docker` — a flux integration plugin for the Docker Engine REST API (v1.43) spoken over the
//! Unix socket (`/var/run/docker.sock`). Every byte crosses the guarded `conn.*` host capability
//! through a hand-rolled HTTP/1.1-over-ConnStream client; the plugin never opens a socket directly.
//!
//! Ops: container (list/show/logs/top/inspect_raw/start/stop/restart/remove/create/run/prune),
//! image (list/show/inspect_raw/pull/tag/remove/prune), network (list/show/inspect_raw/create/
//! remove/prune), volume (list/show/inspect_raw/create/remove/prune), system (info/df).
//!
//! Residuals (require streaming/hijack/long-poll — NOT implemented):
//!   docker.container.exec    — hijacked stdio (upgrade to raw TCP stream not doable one-shot)
//!   docker.container.stats   — streaming JSON (no termination signal on one-shot)
//!   docker.container.logs follow=true — same streaming issue
//!   docker.image.build       — tar upload + streamed NDJSON build log
//!   docker.image.push        — streamed NDJSON progress
//!   docker.events            — long-poll event stream

use host_kit::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};

// ===========================================================================
// Schema-only op input structs (D-36)
// ===========================================================================
// Each op's `input_schema` is derived from the structs below via schemars
// (`host_kit::read_op_typed::<T>` / `write_op_typed::<T>`), instead of a
// hand-written `json!({...})` object via the local `so()` helper. The structs are
// schema-only: handlers keep their existing `opt_str` / `opt_i64` / `opt_bool`
// extractors (the D-34 schema-only precedent).

/// Shared per-call Docker daemon socket override.
///
/// Architectural split: fluxplane resolves the Docker daemon through plugin
/// context (`host.endpoint(...)` / `host.conn_dial(...)`); flux exposes the
/// socket path per-call so callers can target a remote/docker-context
/// endpoint resolved by the host.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct SocketProps {
    /// Docker daemon Unix socket path.
    socket: Option<String>,
}

/// `docker.info`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct InfoInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    socket: SocketProps,
}

/// `docker.system.df`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct SystemDfInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    socket: SocketProps,
    /// Object types to include in disk usage: image, container, volume, build-cache.
    types: Option<Vec<String>>,
}

/// `docker.container.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct ContainerListInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    socket: SocketProps,
    /// Include stopped containers.
    all: Option<bool>,
    /// Maximum containers to return.
    limit: Option<i64>,
    /// Container status filters.
    status: Option<Vec<String>>,
    /// Container name filters.
    name: Option<Vec<String>>,
    /// Container label filters.
    label: Option<Vec<String>>,
}

/// `docker.container.show`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct ContainerShowInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    socket: SocketProps,
    /// Container ID or name.
    id: String,
}

/// `docker.container.logs`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct ContainerLogsInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    socket: SocketProps,
    /// Container ID or name.
    id: String,
    /// Number of log lines to return (default 200).
    tail: Option<i64>,
    /// Show logs since timestamp or duration supported by Docker.
    since: Option<String>,
    /// Show logs until timestamp or duration supported by Docker.
    until: Option<String>,
    /// Include log timestamps.
    timestamps: Option<bool>,
}

/// `docker.container.top`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct ContainerTopInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    socket: SocketProps,
    /// Container ID or name.
    id: String,
    /// Optional `ps` arguments (e.g. `-ef`).
    args: Option<Vec<String>>,
}

/// `docker.container.inspect.raw`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct ContainerInspectRawInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    socket: SocketProps,
    /// Container ID or name.
    id: String,
}

/// `docker.container.start`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct ContainerStartInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    socket: SocketProps,
    /// Container ID or name.
    id: String,
}

/// `docker.container.stop`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct ContainerStopInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    socket: SocketProps,
    /// Container ID or name.
    id: String,
    /// Seconds to wait before killing.
    timeout: Option<i64>,
    /// Signal to send before killing, for example SIGTERM.
    signal: Option<String>,
}

/// `docker.container.restart`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct ContainerRestartInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    socket: SocketProps,
    /// Container ID or name.
    id: String,
    /// Seconds to wait before killing during restart.
    timeout: Option<i64>,
    /// Signal to send before killing, for example SIGTERM.
    signal: Option<String>,
}

/// `docker.container.remove`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct ContainerRemoveInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    socket: SocketProps,
    /// Container ID or name.
    id: String,
    /// Force removal of a running container.
    force: Option<bool>,
    /// Remove anonymous volumes associated with the container.
    volumes: Option<bool>,
}

/// `docker.container.create` and `docker.container.run`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct ContainerCreateInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    socket: SocketProps,
    /// Image to create the container from.
    image: String,
    /// Container name.
    name: Option<String>,
    /// Container command argv.
    cmd: Option<Vec<String>>,
    /// Container entrypoint argv.
    entrypoint: Option<Vec<String>>,
    /// Environment variables in KEY=value form.
    env: Option<Vec<String>>,
    /// Container labels.
    labels: Option<Value>,
    /// Working directory inside the container.
    workdir: Option<String>,
    /// User to run as.
    user: Option<String>,
    /// Container hostname.
    hostname: Option<String>,
    /// Network mode or network name.
    network: Option<String>,
    /// Restart policy: no, always, on-failure, unless-stopped.
    restart: Option<String>,
    /// Automatically remove the container when it exits.
    auto_remove: Option<bool>,
    /// Allocate a TTY.
    tty: Option<bool>,
    /// Keep stdin open.
    open_stdin: Option<bool>,
    /// Run container in privileged mode.
    privileged: Option<bool>,
    /// Bind mounts in Docker -v syntax.
    binds: Option<Vec<String>>,
    /// Structured mounts.
    mounts: Option<Vec<Value>>,
    /// Port bindings.
    ports: Option<Vec<Value>>,
    /// Image platform, for example linux/amd64.
    platform: Option<String>,
}

/// `docker.container.prune`, `docker.network.prune`, `docker.volume.prune`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct PruneInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    socket: SocketProps,
    /// Prune objects created before this timestamp or duration.
    until: Option<String>,
    /// Only prune objects with these labels.
    label: Option<Vec<String>>,
}

/// `docker.image.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct ImageListInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    socket: SocketProps,
    /// Include intermediate images.
    all: Option<bool>,
    /// Maximum images to return.
    limit: Option<i64>,
    /// Image reference filters.
    reference: Option<Vec<String>>,
    /// Image label filters.
    label: Option<Vec<String>>,
}

/// `docker.image.show`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct ImageShowInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    socket: SocketProps,
    /// Image ID, digest, or reference.
    id: String,
}

/// `docker.image.inspect.raw`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct ImageInspectRawInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    socket: SocketProps,
    /// Image ID, digest, or reference.
    id: String,
}

/// `docker.image.pull`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct ImagePullInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    socket: SocketProps,
    /// Image reference to pull.
    reference: String,
    /// Optional platform, for example linux/amd64.
    platform: Option<String>,
    /// Maximum pull progress events to return.
    limit: Option<i64>,
}

/// `docker.image.tag`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct ImageTagInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    socket: SocketProps,
    /// Source image ID or reference.
    source: String,
    /// Target image reference.
    target: String,
}

/// `docker.image.remove`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct ImageRemoveInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    socket: SocketProps,
    /// Image ID, digest, or reference.
    id: String,
    /// Force image removal.
    force: Option<bool>,
    /// Do not prune untagged parent images.
    noprune: Option<bool>,
}

/// `docker.image.prune`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct ImagePruneInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    socket: SocketProps,
    /// Prune all unused images, not only dangling images.
    all: Option<bool>,
    /// Prune images created before this timestamp or duration.
    until: Option<String>,
    /// Only prune images with these labels.
    label: Option<Vec<String>>,
}

/// `docker.network.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct NetworkListInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    socket: SocketProps,
    /// Maximum networks to return.
    limit: Option<i64>,
    /// Network name filters.
    name: Option<Vec<String>>,
    /// Network label filters.
    label: Option<Vec<String>>,
}

/// `docker.network.show`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct NetworkShowInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    socket: SocketProps,
    /// Network ID or name.
    id: String,
}

/// `docker.network.inspect.raw`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct NetworkInspectRawInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    socket: SocketProps,
    /// Network ID or name.
    id: String,
}

/// `docker.network.create`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct NetworkCreateInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    socket: SocketProps,
    /// Network name.
    name: String,
    /// Network driver.
    driver: Option<String>,
    /// Network scope.
    scope: Option<String>,
    /// Restrict external access to the network.
    internal: Option<bool>,
    /// Allow standalone containers to attach to swarm-scoped network.
    attachable: Option<bool>,
    /// Create ingress routing mesh network.
    ingress: Option<bool>,
    /// Enable IPv4.
    enable_ipv4: Option<bool>,
    /// Enable IPv6.
    enable_ipv6: Option<bool>,
    /// Driver options.
    options: Option<Value>,
    /// Network labels.
    labels: Option<Value>,
}

/// `docker.network.remove`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct NetworkRemoveInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    socket: SocketProps,
    /// Network ID or name.
    id: String,
}

/// `docker.volume.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct VolumeListInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    socket: SocketProps,
    /// Maximum volumes to return.
    limit: Option<i64>,
    /// Volume name filters.
    name: Option<Vec<String>>,
    /// Volume label filters.
    label: Option<Vec<String>>,
}

/// `docker.volume.show`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct VolumeShowInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    socket: SocketProps,
    /// Volume name.
    id: String,
}

/// `docker.volume.inspect.raw`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct VolumeInspectRawInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    socket: SocketProps,
    /// Volume name.
    id: String,
}

/// `docker.volume.create`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct VolumeCreateInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    socket: SocketProps,
    /// Volume name. Empty lets Docker generate one.
    name: Option<String>,
    /// Volume driver.
    driver: Option<String>,
    /// Volume driver options.
    driver_opts: Option<Value>,
    /// Volume labels.
    labels: Option<Value>,
}

/// `docker.volume.remove`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct VolumeRemoveInput {
    #[serde(flatten)]
    #[schemars(flatten)]
    socket: SocketProps,
    /// Volume name.
    id: String,
    /// Force volume removal.
    force: Option<bool>,
}

// ---------------------------------------------------------------------------
// HTTP/1.1 over ConnStream — one connection per request, Connection: close.
// ---------------------------------------------------------------------------

fn docker_request(
    host: &mut Host,
    sock: &str,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    content_type: Option<&str>,
) -> Result<(u16, Vec<u8>), String> {
    let cid = host.conn_dial(ConnTarget::Unix { path: sock })?;
    let result = (|| -> Result<(u16, Vec<u8>), String> {
        let body_bytes = body.unwrap_or(&[]);
        let mut req =
            format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n");
        if !body_bytes.is_empty() {
            let ct = content_type.unwrap_or("application/json");
            req.push_str(&format!(
                "Content-Type: {ct}\r\nContent-Length: {}\r\n",
                body_bytes.len()
            ));
        }
        req.push_str("\r\n");

        let stream = ConnStream::new(host, cid);
        let mut reader = BufReader::new(stream);
        reader
            .get_mut()
            .write_all(req.as_bytes())
            .map_err(|e| e.to_string())?;
        if !body_bytes.is_empty() {
            reader
                .get_mut()
                .write_all(body_bytes)
                .map_err(|e| e.to_string())?;
        }

        // Read status line.
        let mut status_line = String::new();
        reader
            .read_line(&mut status_line)
            .map_err(|e| e.to_string())?;
        let status: u16 = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| format!("bad HTTP status line: {status_line:?}"))?;

        // Read headers.
        let mut content_length: Option<usize> = None;
        let mut chunked = false;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).map_err(|e| e.to_string())?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            let lower = trimmed.to_ascii_lowercase();
            if lower.starts_with("content-length:") {
                if let Some((_, v)) = trimmed.split_once(':') {
                    content_length = v.trim().parse().ok();
                }
            } else if lower.starts_with("transfer-encoding:") && lower.contains("chunked") {
                chunked = true;
            }
        }

        // Read body.
        let body_bytes = if chunked {
            read_chunked(&mut reader)?
        } else if let Some(len) = content_length {
            let mut buf = vec![0u8; len];
            reader.read_exact(&mut buf).map_err(|e| e.to_string())?;
            buf
        } else {
            let mut buf = Vec::new();
            reader.read_to_end(&mut buf).map_err(|e| e.to_string())?;
            buf
        };

        Ok((status, body_bytes))
    })();
    host.conn_close(cid)?;
    result
}

fn read_chunked(reader: &mut BufReader<ConnStream>) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let mut size_line = String::new();
        reader
            .read_line(&mut size_line)
            .map_err(|e| e.to_string())?;
        let size_str = size_line.trim().split(';').next().unwrap_or("0");
        let chunk_size =
            usize::from_str_radix(size_str, 16).map_err(|e| format!("bad chunk size: {e}"))?;
        if chunk_size == 0 {
            // Consume trailing CRLF after the zero chunk.
            let mut trailing = String::new();
            reader.read_line(&mut trailing).map_err(|e| e.to_string())?;
            break;
        }
        let mut chunk = vec![0u8; chunk_size];
        reader.read_exact(&mut chunk).map_err(|e| e.to_string())?;
        out.extend_from_slice(&chunk);
        // Consume CRLF after chunk data.
        let mut crlf = String::new();
        reader.read_line(&mut crlf).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

fn docker_json(
    host: &mut Host,
    sock: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<Value, String> {
    let body_bytes;
    let (body_ref, ct) = match body {
        Some(v) => {
            body_bytes = serde_json::to_vec(v).map_err(|e| e.to_string())?;
            (Some(body_bytes.as_slice()), Some("application/json"))
        }
        None => (None, None),
    };
    let (status, bytes) = docker_request(host, sock, method, path, body_ref, ct)?;
    if status == 204 || bytes.is_empty() {
        return Ok(json!({"ok": true, "status": status}));
    }
    if !(200..300).contains(&(status as u32)) {
        let msg = String::from_utf8_lossy(&bytes);
        return Err(format!("docker {method} {path} → {status}: {msg}"));
    }
    serde_json::from_slice(&bytes).map_err(|e| format!("docker: bad JSON from {path}: {e}"))
}

/// URL-encode a path segment (e.g. image names with slashes/colons).
fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn get_sock(input: &Value) -> String {
    input
        .get("socket")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("/var/run/docker.sock")
        .to_string()
}

fn req_str(input: &Value, key: &str) -> Result<String, String> {
    input
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
        .ok_or_else(|| format!("`{key}` (string) required"))
}

fn opt_str(input: &Value, key: &str) -> Option<String> {
    input
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
}

fn opt_bool(input: &Value, key: &str) -> Option<bool> {
    input.get(key).and_then(|v| v.as_bool())
}

fn opt_i64(input: &Value, key: &str) -> Option<i64> {
    input.get(key).and_then(|v| v.as_i64())
}

/// Build a `?k=v&...` query string from non-empty pairs.
fn qs(pairs: &[(&str, String)]) -> String {
    let parts: Vec<String> = pairs
        .iter()
        .filter(|(_, v)| !v.is_empty())
        .map(|(k, v)| format!("{k}={}", enc(v)))
        .collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!("?{}", parts.join("&"))
    }
}

fn short_id(id: &str) -> String {
    let id = id.trim_start_matches("sha256:");
    if id.len() <= 12 {
        id.to_string()
    } else {
        id[..12].to_string()
    }
}

// ---------------------------------------------------------------------------
// Manifest builder.
// ---------------------------------------------------------------------------

fn manifest_builder() -> PluginBuilder {
    PluginBuilder::new("docker", "0.1.0")
        .capabilities(Caps {
            conn: vec![
                "unix:/var/run/docker.sock".into(),
                "unix:/var/run/*.sock".into(),
            ],
            ..Default::default()
        })
        .datasource(ds(
            "docker.containers",
            "docker.container",
            "Docker containers.",
        ))
        .datasource(ds("docker.images", "docker.image", "Docker images."))
        .datasource(ds("docker.networks", "docker.network", "Docker networks."))
        .datasource(ds("docker.volumes", "docker.volume", "Docker volumes."))
        // ---- system ----
        .operation(
            read_op_typed::<InfoInput>("docker.info", "Show Docker daemon and server information."),
            system_info,
        )
        .operation(
            read_op_typed::<SystemDfInput>(
                "docker.system.df",
                "Show Docker disk usage by object type.",
            ),
            system_df,
        )
        // ---- containers ----
        .operation(
            read_op_typed::<ContainerListInput>("docker.container.list", "List Docker containers."),
            container_list,
        )
        .operation(
            read_op_typed::<ContainerShowInput>(
                "docker.container.show",
                "Show one Docker container by ID or name.",
            ),
            container_show,
        )
        .operation(
            read_op_typed::<ContainerLogsInput>(
                "docker.container.logs",
                "Read recent Docker container logs (non-streaming).",
            ),
            container_logs,
        )
        .operation(
            read_op_typed::<ContainerTopInput>(
                "docker.container.top",
                "Show processes running inside a Docker container.",
            ),
            container_top,
        )
        .operation(
            read_op_typed::<ContainerInspectRawInput>(
                "docker.container.inspect.raw",
                "Show raw Docker container inspect data.",
            ),
            container_inspect_raw,
        )
        .operation(
            {
                let mut op = write_op_typed::<ContainerStartInput>(
                    "docker.container.start",
                    "Start a Docker container.",
                );
                op.risk = Some(Risk::Medium);
                op
            },
            container_start,
        )
        .operation(
            {
                let mut op = write_op_typed::<ContainerStopInput>(
                    "docker.container.stop",
                    "Stop a Docker container.",
                );
                op.risk = Some(Risk::Medium);
                op
            },
            container_stop,
        )
        .operation(
            {
                let mut op = write_op_typed::<ContainerRestartInput>(
                    "docker.container.restart",
                    "Restart a Docker container.",
                );
                op.risk = Some(Risk::Medium);
                op
            },
            container_restart,
        )
        .operation(
            {
                let mut op = write_op_typed::<ContainerRemoveInput>(
                    "docker.container.remove",
                    "Remove a Docker container.",
                );
                op.risk = Some(Risk::Destructive);
                op
            },
            container_remove,
        )
        .operation(
            {
                let mut op = write_op_typed::<ContainerCreateInput>(
                    "docker.container.create",
                    "Create a Docker container.",
                );
                op.risk = Some(Risk::Medium);
                op
            },
            container_create,
        )
        .operation(
            {
                let mut op = write_op_typed::<ContainerCreateInput>(
                    "docker.container.run",
                    "Create and start a Docker container.",
                );
                op.risk = Some(Risk::Medium);
                op
            },
            container_run,
        )
        .operation(
            {
                let mut op = write_op_typed::<PruneInput>(
                    "docker.container.prune",
                    "Prune stopped Docker containers.",
                );
                op.risk = Some(Risk::Destructive);
                op
            },
            container_prune,
        )
        // ---- images ----
        .operation(
            read_op_typed::<ImageListInput>("docker.image.list", "List local Docker images."),
            image_list,
        )
        .operation(
            read_op_typed::<ImageShowInput>(
                "docker.image.show",
                "Show one Docker image by ID, digest, or reference.",
            ),
            image_show,
        )
        .operation(
            read_op_typed::<ImageInspectRawInput>(
                "docker.image.inspect.raw",
                "Show raw Docker image inspect data.",
            ),
            image_inspect_raw,
        )
        .operation(
            {
                let mut op =
                    write_op_typed::<ImagePullInput>("docker.image.pull", "Pull a Docker image.");
                op.risk = Some(Risk::Medium);
                op
            },
            image_pull,
        )
        .operation(
            {
                let mut op =
                    write_op_typed::<ImageTagInput>("docker.image.tag", "Tag a Docker image.");
                op.risk = Some(Risk::Medium);
                op
            },
            image_tag,
        )
        .operation(
            {
                let mut op = write_op_typed::<ImageRemoveInput>(
                    "docker.image.remove",
                    "Remove a Docker image.",
                );
                op.risk = Some(Risk::Destructive);
                op
            },
            image_remove,
        )
        .operation(
            {
                let mut op = write_op_typed::<ImagePruneInput>(
                    "docker.image.prune",
                    "Prune unused Docker images.",
                );
                op.risk = Some(Risk::Destructive);
                op
            },
            image_prune,
        )
        // ---- networks ----
        .operation(
            read_op_typed::<NetworkListInput>("docker.network.list", "List Docker networks."),
            network_list,
        )
        .operation(
            read_op_typed::<NetworkShowInput>(
                "docker.network.show",
                "Show one Docker network by ID or name.",
            ),
            network_show,
        )
        .operation(
            read_op_typed::<NetworkInspectRawInput>(
                "docker.network.inspect.raw",
                "Show raw Docker network inspect data.",
            ),
            network_inspect_raw,
        )
        .operation(
            {
                let mut op = write_op_typed::<NetworkCreateInput>(
                    "docker.network.create",
                    "Create a Docker network.",
                );
                op.risk = Some(Risk::Medium);
                op
            },
            network_create,
        )
        .operation(
            {
                let mut op = write_op_typed::<NetworkRemoveInput>(
                    "docker.network.remove",
                    "Remove a Docker network.",
                );
                op.risk = Some(Risk::Destructive);
                op
            },
            network_remove,
        )
        .operation(
            {
                let mut op = write_op_typed::<PruneInput>(
                    "docker.network.prune",
                    "Prune unused Docker networks.",
                );
                op.risk = Some(Risk::Destructive);
                op
            },
            network_prune,
        )
        // ---- volumes ----
        .operation(
            read_op_typed::<VolumeListInput>("docker.volume.list", "List Docker volumes."),
            volume_list,
        )
        .operation(
            read_op_typed::<VolumeShowInput>(
                "docker.volume.show",
                "Show one Docker volume by name.",
            ),
            volume_show,
        )
        .operation(
            read_op_typed::<VolumeInspectRawInput>(
                "docker.volume.inspect.raw",
                "Show raw Docker volume inspect data.",
            ),
            volume_inspect_raw,
        )
        .operation(
            {
                let mut op = write_op_typed::<VolumeCreateInput>(
                    "docker.volume.create",
                    "Create a Docker volume.",
                );
                op.risk = Some(Risk::Medium);
                op
            },
            volume_create,
        )
        .operation(
            {
                let mut op = write_op_typed::<VolumeRemoveInput>(
                    "docker.volume.remove",
                    "Remove a Docker volume.",
                );
                op.risk = Some(Risk::Destructive);
                op
            },
            volume_remove,
        )
        .operation(
            {
                let mut op = write_op_typed::<PruneInput>(
                    "docker.volume.prune",
                    "Prune unused Docker volumes.",
                );
                op.risk = Some(Risk::Destructive);
                op
            },
            volume_prune,
        )
}

fn ds(name: &str, entity: &str, desc: &str) -> Declaration {
    Declaration {
        name: name.into(),
        entity: entity.into(),
        description: Some(desc.into()),
        capabilities: vec!["search".into(), "get".into(), "index".into()],
        entity_schema: None,
    }
}

// ---------------------------------------------------------------------------
// Filter helpers for Docker API query strings.
// ---------------------------------------------------------------------------

/// Build a Docker API JSON filters map from an array field in `input`.
/// Docker /containers/json, /images/json etc. accept `filters={"name":["foo"]}`
fn filters_qs(input: &Value, keys: &[(&str, &str)]) -> String {
    let mut map = serde_json::Map::new();
    for (input_key, filter_key) in keys {
        if let Some(arr) = input.get(*input_key).and_then(|v| v.as_array()) {
            if !arr.is_empty() {
                map.insert((*filter_key).to_string(), Value::Array(arr.clone()));
            }
        }
    }
    if map.is_empty() {
        String::new()
    } else {
        enc(&serde_json::to_string(&Value::Object(map)).unwrap_or_default())
    }
}

// ---------------------------------------------------------------------------
// System ops.
// ---------------------------------------------------------------------------

fn system_info(input: Value, host: &mut Host) -> Result<Value, String> {
    let sock = get_sock(&input);
    let raw = docker_json(host, &sock, "GET", "/v1.43/info", None)?;
    Ok(json!({
        "id": raw.get("ID").cloned().unwrap_or(Value::Null),
        "name": raw.get("Name").cloned().unwrap_or(Value::Null),
        "server_version": raw.get("ServerVersion").cloned().unwrap_or(Value::Null),
        "os_type": raw.get("OSType").cloned().unwrap_or(Value::Null),
        "operating_system": raw.get("OperatingSystem").cloned().unwrap_or(Value::Null),
        "architecture": raw.get("Architecture").cloned().unwrap_or(Value::Null),
        "kernel_version": raw.get("KernelVersion").cloned().unwrap_or(Value::Null),
        "containers": raw.get("Containers").cloned().unwrap_or(json!(0)),
        "containers_running": raw.get("ContainersRunning").cloned().unwrap_or(json!(0)),
        "containers_paused": raw.get("ContainersPaused").cloned().unwrap_or(json!(0)),
        "containers_stopped": raw.get("ContainersStopped").cloned().unwrap_or(json!(0)),
        "images": raw.get("Images").cloned().unwrap_or(json!(0)),
        "cpus": raw.get("NCPU").cloned().unwrap_or(Value::Null),
        "memory_bytes": raw.get("MemTotal").cloned().unwrap_or(Value::Null),
        "docker_root_dir": raw.get("DockerRootDir").cloned().unwrap_or(Value::Null),
        "warnings": raw.get("Warnings").cloned().unwrap_or(json!([])),
    }))
}

fn system_df(input: Value, host: &mut Host) -> Result<Value, String> {
    let sock = get_sock(&input);
    let mut pairs: Vec<(&str, String)> = Vec::new();
    if let Some(types) = input.get("types").and_then(|v| v.as_array()) {
        if !types.is_empty() {
            let mut map = serde_json::Map::new();
            for t in types {
                if let Some(s) = t.as_str() {
                    map.insert(s.to_string(), json!(true));
                }
            }
            pairs.push((
                "type",
                serde_json::to_string(&Value::Object(map)).unwrap_or_default(),
            ));
        }
    }
    let path = format!("/v1.43/system/df{}", qs(&pairs));
    docker_json(host, &sock, "GET", &path, None)
}

// ---------------------------------------------------------------------------
// Container ops.
// ---------------------------------------------------------------------------

fn container_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let sock = get_sock(&input);
    let all = opt_bool(&input, "all").unwrap_or(false);
    let limit = opt_i64(&input, "limit").unwrap_or(100).min(1000);
    let filters = filters_qs(
        &input,
        &[("status", "status"), ("name", "name"), ("label", "label")],
    );
    let path = format!(
        "/v1.43/containers/json{}",
        qs(&[
            ("all", if all { "1".into() } else { String::new() }),
            ("limit", limit.to_string()),
            ("filters", filters),
        ])
    );
    let raw = docker_json(host, &sock, "GET", &path, None)?;
    let arr = raw.as_array().cloned().unwrap_or_default();
    let containers: Vec<Value> = arr.iter().map(normalize_container).collect();
    contribute_containers(host, &containers);
    Ok(Value::Array(containers))
}

fn container_show(input: Value, host: &mut Host) -> Result<Value, String> {
    let sock = get_sock(&input);
    let id = req_str(&input, "id")?;
    let raw = docker_json(
        host,
        &sock,
        "GET",
        &format!("/v1.43/containers/{}/json", enc(&id)),
        None,
    )?;
    Ok(normalize_container_inspect(&raw))
}

fn container_logs(input: Value, host: &mut Host) -> Result<Value, String> {
    let sock = get_sock(&input);
    let id = req_str(&input, "id")?;
    let tail = opt_i64(&input, "tail").unwrap_or(200);
    let pairs = [
        ("stdout", "1".into()),
        ("stderr", "1".into()),
        ("follow", "0".into()),
        ("tail", tail.to_string()),
        ("since", opt_str(&input, "since").unwrap_or_default()),
        ("until", opt_str(&input, "until").unwrap_or_default()),
        (
            "timestamps",
            if opt_bool(&input, "timestamps").unwrap_or(false) {
                "1".into()
            } else {
                String::new()
            },
        ),
    ];
    let path = format!("/v1.43/containers/{}/logs{}", enc(&id), qs(&pairs));
    let (status, raw_bytes) = docker_request(host, &sock, "GET", &path, None, None)?;
    if !(200..300).contains(&(status as u32)) {
        let msg = String::from_utf8_lossy(&raw_bytes);
        return Err(format!("docker GET {path} → {status}: {msg}"));
    }
    // Docker log stream uses an 8-byte multiplexed framing: [stream_type(1) pad(3) size(4)]
    // We strip the framing headers to extract text.
    let text = strip_docker_log_frames(&raw_bytes);
    Ok(json!({ "container": id, "text": text, "tail": tail }))
}

/// Strip Docker multiplexed log stream frames (8-byte header per chunk).
fn strip_docker_log_frames(data: &[u8]) -> String {
    let mut out = String::new();
    let mut pos = 0;
    while pos + 8 <= data.len() {
        let size = u32::from_be_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]])
            as usize;
        pos += 8;
        if pos + size <= data.len() {
            if let Ok(s) = std::str::from_utf8(&data[pos..pos + size]) {
                out.push_str(s);
            }
            pos += size;
        } else {
            break;
        }
    }
    // Fallback: if no frames were parsed, treat as raw UTF-8.
    if out.is_empty() && !data.is_empty() {
        out = String::from_utf8_lossy(data).into_owned();
    }
    out
}

fn container_top(input: Value, host: &mut Host) -> Result<Value, String> {
    let sock = get_sock(&input);
    let id = req_str(&input, "id")?;
    let ps_args = if let Some(args) = input.get("args").and_then(|v| v.as_array()) {
        args.iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        opt_str(&input, "args").unwrap_or_default()
    };
    let path = format!(
        "/v1.43/containers/{}/top{}",
        enc(&id),
        qs(&[("ps_args", ps_args)])
    );
    let raw = docker_json(host, &sock, "GET", &path, None)?;
    let titles: Vec<Value> = raw
        .get("Titles")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let processes: Vec<Value> = raw
        .get("Processes")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let count = processes.len();
    Ok(json!({ "container": id, "titles": titles, "processes": processes, "count": count }))
}

fn container_inspect_raw(input: Value, host: &mut Host) -> Result<Value, String> {
    let sock = get_sock(&input);
    let id = req_str(&input, "id")?;
    let data = docker_json(
        host,
        &sock,
        "GET",
        &format!("/v1.43/containers/{}/json", enc(&id)),
        None,
    )?;
    Ok(json!({ "kind": "container", "id": id, "data": data }))
}

fn container_start(input: Value, host: &mut Host) -> Result<Value, String> {
    let sock = get_sock(&input);
    let id = req_str(&input, "id")?;
    docker_json(
        host,
        &sock,
        "POST",
        &format!("/v1.43/containers/{}/start", enc(&id)),
        Some(&json!({})),
    )?;
    Ok(json!({ "container": id, "action": "start", "ok": true }))
}

fn container_stop(input: Value, host: &mut Host) -> Result<Value, String> {
    let sock = get_sock(&input);
    let id = req_str(&input, "id")?;
    let mut pairs = Vec::new();
    if let Some(t) = opt_i64(&input, "timeout") {
        pairs.push(("t", t.to_string()));
    }
    if let Some(sig) = opt_str(&input, "signal") {
        pairs.push(("signal", sig));
    }
    let path = format!(
        "/v1.43/containers/{}/stop{}",
        enc(&id),
        qs(&pairs
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect::<Vec<_>>())
    );
    docker_json(host, &sock, "POST", &path, Some(&json!({})))?;
    Ok(json!({ "container": id, "action": "stop", "ok": true }))
}

fn container_restart(input: Value, host: &mut Host) -> Result<Value, String> {
    let sock = get_sock(&input);
    let id = req_str(&input, "id")?;
    let mut pairs = Vec::new();
    if let Some(t) = opt_i64(&input, "timeout") {
        pairs.push(("t", t.to_string()));
    }
    if let Some(sig) = opt_str(&input, "signal") {
        pairs.push(("signal", sig));
    }
    let path = format!(
        "/v1.43/containers/{}/restart{}",
        enc(&id),
        qs(&pairs
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect::<Vec<_>>())
    );
    docker_json(host, &sock, "POST", &path, Some(&json!({})))?;
    Ok(json!({ "container": id, "action": "restart", "ok": true }))
}

fn container_remove(input: Value, host: &mut Host) -> Result<Value, String> {
    let sock = get_sock(&input);
    let id = req_str(&input, "id")?;
    let force = opt_bool(&input, "force").unwrap_or(false);
    let volumes = opt_bool(&input, "volumes").unwrap_or(false);
    let path = format!(
        "/v1.43/containers/{}{}",
        enc(&id),
        qs(&[
            ("force", if force { "1".into() } else { String::new() }),
            ("v", if volumes { "1".into() } else { String::new() }),
        ])
    );
    docker_json(host, &sock, "DELETE", &path, None)?;
    Ok(json!({ "container": id, "action": "remove", "ok": true }))
}

fn build_container_body(input: &Value) -> Value {
    let mut host_config = serde_json::Map::new();
    if let Some(binds) = input.get("binds").and_then(|v| v.as_array()) {
        host_config.insert("Binds".into(), Value::Array(binds.clone()));
    }
    if let Some(restart) = opt_str(input, "restart") {
        host_config.insert(
            "RestartPolicy".into(),
            json!({"Name": restart, "MaximumRetryCount": 0}),
        );
    }
    if opt_bool(input, "auto_remove").unwrap_or(false) {
        host_config.insert("AutoRemove".into(), json!(true));
    }
    if opt_bool(input, "privileged").unwrap_or(false) {
        host_config.insert("Privileged".into(), json!(true));
    }
    // Mounts.
    if let Some(mounts) = input.get("mounts").and_then(|v| v.as_array()) {
        let mapped: Vec<Value> = mounts
            .iter()
            .map(|m| {
                json!({
                    "Type": m.get("type").and_then(|v| v.as_str()).unwrap_or("bind"),
                    "Source": m.get("source").and_then(|v| v.as_str()).unwrap_or(""),
                    "Target": m.get("target").and_then(|v| v.as_str()).unwrap_or(""),
                    "ReadOnly": m.get("read_only").and_then(|v| v.as_bool()).unwrap_or(false),
                })
            })
            .collect();
        host_config.insert("Mounts".into(), Value::Array(mapped));
    }

    // Port bindings.
    if let Some(ports) = input.get("ports").and_then(|v| v.as_array()) {
        let mut bindings = serde_json::Map::new();
        let mut exposed = serde_json::Map::new();
        for p in ports {
            let container_port = p
                .get("container")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let host_port = p
                .get("host_port")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let host_ip = p
                .get("host_ip")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let protocol = p
                .get("protocol")
                .and_then(|v| v.as_str())
                .unwrap_or("tcp")
                .to_string();
            let key = if container_port.contains('/') {
                container_port.clone()
            } else {
                format!("{container_port}/{protocol}")
            };
            exposed.insert(key.clone(), json!({}));
            let bind = json!([{"HostIp": host_ip, "HostPort": host_port}]);
            bindings.insert(key, bind);
        }
        host_config.insert("PortBindings".into(), Value::Object(bindings));
        let mut body_exposed = serde_json::Map::new();
        body_exposed.extend(exposed);
        // Will be added to top-level body below.
        host_config.insert("_ExposedPorts".into(), Value::Object(body_exposed));
    }

    let mut body = serde_json::Map::new();
    if let Some(image) = opt_str(input, "image") {
        body.insert("Image".into(), json!(image));
    }
    if let Some(name) = opt_str(input, "name") {
        body.insert("name".into(), json!(name)); // passed as query param but also in body for clarity
    }
    if let Some(cmd) = input.get("cmd").and_then(|v| v.as_array()) {
        body.insert("Cmd".into(), Value::Array(cmd.clone()));
    }
    if let Some(ep) = input.get("entrypoint").and_then(|v| v.as_array()) {
        body.insert("Entrypoint".into(), Value::Array(ep.clone()));
    }
    if let Some(env) = input.get("env").and_then(|v| v.as_array()) {
        body.insert("Env".into(), Value::Array(env.clone()));
    }
    if let Some(labels) = input.get("labels") {
        body.insert("Labels".into(), labels.clone());
    }
    if let Some(wd) = opt_str(input, "workdir") {
        body.insert("WorkingDir".into(), json!(wd));
    }
    if let Some(user) = opt_str(input, "user") {
        body.insert("User".into(), json!(user));
    }
    if let Some(hostname) = opt_str(input, "hostname") {
        body.insert("Hostname".into(), json!(hostname));
    }
    if let Some(ep_ports) = host_config.remove("_ExposedPorts") {
        body.insert("ExposedPorts".into(), ep_ports);
    }
    if opt_bool(input, "tty").unwrap_or(false) {
        body.insert("Tty".into(), json!(true));
    }
    if opt_bool(input, "open_stdin").unwrap_or(false) {
        body.insert("OpenStdin".into(), json!(true));
    }
    if let Some(network) = opt_str(input, "network") {
        host_config.insert("NetworkMode".into(), json!(network));
    }
    body.insert("HostConfig".into(), Value::Object(host_config));
    Value::Object(body)
}

fn container_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let sock = get_sock(&input);
    let name = opt_str(&input, "name").unwrap_or_default();
    let path = format!(
        "/v1.43/containers/create{}",
        qs(&[
            ("name", name),
            ("platform", opt_str(&input, "platform").unwrap_or_default())
        ])
    );
    let body = build_container_body(&input);
    let resp = docker_json(host, &sock, "POST", &path, Some(&body))?;
    let id = resp
        .get("Id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let warnings = resp.get("Warnings").cloned().unwrap_or(json!([]));
    Ok(json!({ "id": id, "warnings": warnings, "ok": true }))
}

fn container_run(input: Value, host: &mut Host) -> Result<Value, String> {
    let sock = get_sock(&input);
    let name = opt_str(&input, "name").unwrap_or_default();
    let path = format!(
        "/v1.43/containers/create{}",
        qs(&[
            ("name", name),
            ("platform", opt_str(&input, "platform").unwrap_or_default())
        ])
    );
    let body = build_container_body(&input);
    let resp = docker_json(host, &sock, "POST", &path, Some(&body))?;
    let id = resp
        .get("Id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let warnings = resp.get("Warnings").cloned().unwrap_or(json!([]));
    if !id.is_empty() {
        docker_json(
            host,
            &sock,
            "POST",
            &format!("/v1.43/containers/{}/start", enc(&id)),
            Some(&json!({})),
        )?;
    }
    Ok(json!({ "id": id, "started": true, "warnings": warnings, "ok": true }))
}

fn container_prune(input: Value, host: &mut Host) -> Result<Value, String> {
    let sock = get_sock(&input);
    let filters = build_prune_filters(&input);
    let path = format!("/v1.43/containers/prune{}", qs(&[("filters", filters)]));
    let resp = docker_json(host, &sock, "POST", &path, Some(&json!({})))?;
    let deleted: Vec<Value> = resp
        .get("ContainersDeleted")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let space = resp
        .get("SpaceReclaimed")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    Ok(
        json!({ "kind": "container", "deleted": deleted, "space_reclaimed_bytes": space, "count": deleted.len(), "ok": true }),
    )
}

// ---------------------------------------------------------------------------
// Image ops.
// ---------------------------------------------------------------------------

fn image_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let sock = get_sock(&input);
    let all = opt_bool(&input, "all").unwrap_or(false);
    let filters = filters_qs(&input, &[("reference", "reference"), ("label", "label")]);
    let path = format!(
        "/v1.43/images/json{}",
        qs(&[
            ("all", if all { "1".into() } else { String::new() }),
            ("filters", filters),
        ])
    );
    let raw = docker_json(host, &sock, "GET", &path, None)?;
    let arr = raw.as_array().cloned().unwrap_or_default();
    let images: Vec<Value> = arr.iter().map(normalize_image).collect();
    contribute_images(host, &images);
    Ok(Value::Array(images))
}

fn image_show(input: Value, host: &mut Host) -> Result<Value, String> {
    let sock = get_sock(&input);
    let id = req_str(&input, "id")?;
    let raw = docker_json(
        host,
        &sock,
        "GET",
        &format!("/v1.43/images/{}/json", enc(&id)),
        None,
    )?;
    Ok(normalize_image_inspect(&raw))
}

fn image_inspect_raw(input: Value, host: &mut Host) -> Result<Value, String> {
    let sock = get_sock(&input);
    let id = req_str(&input, "id")?;
    let data = docker_json(
        host,
        &sock,
        "GET",
        &format!("/v1.43/images/{}/json", enc(&id)),
        None,
    )?;
    Ok(json!({ "kind": "image", "id": id, "data": data }))
}

fn image_pull(input: Value, host: &mut Host) -> Result<Value, String> {
    let sock = get_sock(&input);
    let reference = req_str(&input, "reference")?;
    let platform = opt_str(&input, "platform").unwrap_or_default();
    let limit = opt_i64(&input, "limit").map(|n| n.max(0) as usize);
    let path = format!(
        "/v1.43/images/create{}",
        qs(&[("fromImage", reference.clone()), ("platform", platform)])
    );
    // image pull returns NDJSON stream; we consume it fully.
    let (status, bytes) = docker_request(host, &sock, "POST", &path, None, None)?;
    if !(200..300).contains(&(status as u32)) {
        return Err(format!("docker image pull {reference} → {status}"));
    }
    let text = String::from_utf8_lossy(&bytes);
    // Count lines as events, honoring the caller-specified limit.
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let count = limit.map(|n| lines.len().min(n)).unwrap_or(lines.len());
    Ok(json!({ "reference": reference, "count": count, "ok": true }))
}

fn image_tag(input: Value, host: &mut Host) -> Result<Value, String> {
    let sock = get_sock(&input);
    let source = req_str(&input, "source")?;
    let target = req_str(&input, "target")?;
    // Parse target into repo + tag.
    let (repo, tag) = if let Some(pos) = target.rfind(':') {
        (&target[..pos], &target[pos + 1..])
    } else {
        (target.as_str(), "latest")
    };
    let path = format!(
        "/v1.43/images/{}/tag{}",
        enc(&source),
        qs(&[("repo", repo.to_string()), ("tag", tag.to_string())])
    );
    docker_json(host, &sock, "POST", &path, Some(&json!({})))?;
    Ok(json!({ "id": source, "action": "tag", "target": target, "ok": true }))
}

fn image_remove(input: Value, host: &mut Host) -> Result<Value, String> {
    let sock = get_sock(&input);
    let id = req_str(&input, "id")?;
    let force = opt_bool(&input, "force").unwrap_or(false);
    let noprune = opt_bool(&input, "noprune").unwrap_or(false);
    let path = format!(
        "/v1.43/images/{}{}",
        enc(&id),
        qs(&[
            ("force", if force { "1".into() } else { String::new() }),
            ("noprune", if noprune { "1".into() } else { String::new() }),
        ])
    );
    let resp = docker_json(host, &sock, "DELETE", &path, None)?;
    let deleted: Vec<String> = resp
        .as_array()
        .iter()
        .flat_map(|a| a.iter())
        .filter_map(|item| {
            item.get("Deleted")
                .or_else(|| item.get("Untagged"))
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .collect();
    Ok(json!({ "id": id, "deleted": deleted, "ok": true }))
}

fn image_prune(input: Value, host: &mut Host) -> Result<Value, String> {
    let sock = get_sock(&input);
    let all = opt_bool(&input, "all").unwrap_or(false);
    let mut filter_map = serde_json::Map::new();
    if all {
        filter_map.insert("dangling".into(), json!(["false"]));
    }
    if let Some(until) = opt_str(&input, "until") {
        filter_map.insert("until".into(), json!([until]));
    }
    if let Some(labels) = input.get("label").and_then(|v| v.as_array()) {
        if !labels.is_empty() {
            filter_map.insert("label".into(), Value::Array(labels.clone()));
        }
    }
    let filters = if filter_map.is_empty() {
        String::new()
    } else {
        enc(&serde_json::to_string(&Value::Object(filter_map)).unwrap_or_default())
    };
    let path = format!("/v1.43/images/prune{}", qs(&[("filters", filters)]));
    let resp = docker_json(host, &sock, "POST", &path, Some(&json!({})))?;
    let deleted: Vec<Value> = resp
        .get("ImagesDeleted")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let space = resp
        .get("SpaceReclaimed")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    Ok(
        json!({ "kind": "image", "deleted": deleted, "space_reclaimed_bytes": space, "count": deleted.len(), "ok": true }),
    )
}

// ---------------------------------------------------------------------------
// Network ops.
// ---------------------------------------------------------------------------

fn network_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let sock = get_sock(&input);
    let limit = opt_i64(&input, "limit").map(|n| n.max(0) as usize);
    let filters = filters_qs(&input, &[("name", "name"), ("label", "label")]);
    let path = format!("/v1.43/networks{}", qs(&[("filters", filters)]));
    let raw = docker_json(host, &sock, "GET", &path, None)?;
    let arr = raw.as_array().cloned().unwrap_or_default();
    let mut networks: Vec<Value> = arr.iter().map(normalize_network).collect();
    if let Some(limit) = limit {
        networks.truncate(limit);
    }
    contribute_networks(host, &networks);
    Ok(Value::Array(networks))
}

fn network_show(input: Value, host: &mut Host) -> Result<Value, String> {
    let sock = get_sock(&input);
    let id = req_str(&input, "id")?;
    let raw = docker_json(
        host,
        &sock,
        "GET",
        &format!("/v1.43/networks/{}", enc(&id)),
        None,
    )?;
    Ok(normalize_network(&raw))
}

fn network_inspect_raw(input: Value, host: &mut Host) -> Result<Value, String> {
    let sock = get_sock(&input);
    let id = req_str(&input, "id")?;
    let data = docker_json(
        host,
        &sock,
        "GET",
        &format!("/v1.43/networks/{}", enc(&id)),
        None,
    )?;
    Ok(json!({ "kind": "network", "id": id, "data": data }))
}

fn network_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let sock = get_sock(&input);
    let name = req_str(&input, "name")?;
    let mut body = serde_json::Map::new();
    body.insert("Name".into(), json!(name));
    if let Some(driver) = opt_str(&input, "driver") {
        body.insert("Driver".into(), json!(driver));
    }
    if let Some(scope) = opt_str(&input, "scope") {
        body.insert("Scope".into(), json!(scope));
    }
    if opt_bool(&input, "internal").unwrap_or(false) {
        body.insert("Internal".into(), json!(true));
    }
    if opt_bool(&input, "attachable").unwrap_or(false) {
        body.insert("Attachable".into(), json!(true));
    }
    if opt_bool(&input, "ingress").unwrap_or(false) {
        body.insert("Ingress".into(), json!(true));
    }
    if let Some(v) = opt_bool(&input, "enable_ipv4") {
        body.insert("EnableIPv4".into(), json!(v));
    }
    if let Some(v) = opt_bool(&input, "enable_ipv6") {
        body.insert("EnableIPv6".into(), json!(v));
    }
    if let Some(opts) = input.get("options") {
        body.insert("Options".into(), opts.clone());
    }
    if let Some(labels) = input.get("labels") {
        body.insert("Labels".into(), labels.clone());
    }
    let resp = docker_json(
        host,
        &sock,
        "POST",
        "/v1.43/networks/create",
        Some(&Value::Object(body)),
    )?;
    let id = resp
        .get("Id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Ok(json!({ "id": id, "action": "create", "ok": true }))
}

fn network_remove(input: Value, host: &mut Host) -> Result<Value, String> {
    let sock = get_sock(&input);
    let id = req_str(&input, "id")?;
    docker_json(
        host,
        &sock,
        "DELETE",
        &format!("/v1.43/networks/{}", enc(&id)),
        None,
    )?;
    Ok(json!({ "id": id, "action": "remove", "ok": true }))
}

fn network_prune(input: Value, host: &mut Host) -> Result<Value, String> {
    let sock = get_sock(&input);
    let filters = build_prune_filters(&input);
    let path = format!("/v1.43/networks/prune{}", qs(&[("filters", filters)]));
    let resp = docker_json(host, &sock, "POST", &path, Some(&json!({})))?;
    let deleted: Vec<Value> = resp
        .get("NetworksDeleted")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(json!({ "kind": "network", "deleted": deleted, "count": deleted.len(), "ok": true }))
}

// ---------------------------------------------------------------------------
// Volume ops.
// ---------------------------------------------------------------------------

fn volume_list(input: Value, host: &mut Host) -> Result<Value, String> {
    let sock = get_sock(&input);
    let limit = opt_i64(&input, "limit").map(|n| n.max(0) as usize);
    let filters = filters_qs(&input, &[("name", "name"), ("label", "label")]);
    let path = format!("/v1.43/volumes{}", qs(&[("filters", filters)]));
    let resp = docker_json(host, &sock, "GET", &path, None)?;
    let arr = resp
        .get("Volumes")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut volumes: Vec<Value> = arr.iter().map(normalize_volume).collect();
    if let Some(limit) = limit {
        volumes.truncate(limit);
    }
    contribute_volumes(host, &volumes);
    Ok(Value::Array(volumes))
}

fn volume_show(input: Value, host: &mut Host) -> Result<Value, String> {
    let sock = get_sock(&input);
    let id = req_str(&input, "id")?;
    let raw = docker_json(
        host,
        &sock,
        "GET",
        &format!("/v1.43/volumes/{}", enc(&id)),
        None,
    )?;
    Ok(normalize_volume(&raw))
}

fn volume_inspect_raw(input: Value, host: &mut Host) -> Result<Value, String> {
    let sock = get_sock(&input);
    let id = req_str(&input, "id")?;
    let data = docker_json(
        host,
        &sock,
        "GET",
        &format!("/v1.43/volumes/{}", enc(&id)),
        None,
    )?;
    Ok(json!({ "kind": "volume", "id": id, "data": data }))
}

fn volume_create(input: Value, host: &mut Host) -> Result<Value, String> {
    let sock = get_sock(&input);
    let mut body = serde_json::Map::new();
    if let Some(name) = opt_str(&input, "name") {
        body.insert("Name".into(), json!(name));
    }
    if let Some(driver) = opt_str(&input, "driver") {
        body.insert("Driver".into(), json!(driver));
    }
    if let Some(opts) = input.get("driver_opts") {
        body.insert("DriverOpts".into(), opts.clone());
    }
    if let Some(labels) = input.get("labels") {
        body.insert("Labels".into(), labels.clone());
    }
    let resp = docker_json(
        host,
        &sock,
        "POST",
        "/v1.43/volumes/create",
        Some(&Value::Object(body)),
    )?;
    Ok(normalize_volume(&resp))
}

fn volume_remove(input: Value, host: &mut Host) -> Result<Value, String> {
    let sock = get_sock(&input);
    let id = req_str(&input, "id")?;
    let force = opt_bool(&input, "force").unwrap_or(false);
    let path = format!(
        "/v1.43/volumes/{}{}",
        enc(&id),
        qs(&[("force", if force { "1".into() } else { String::new() })])
    );
    docker_json(host, &sock, "DELETE", &path, None)?;
    Ok(json!({ "id": id, "action": "remove", "ok": true }))
}

fn volume_prune(input: Value, host: &mut Host) -> Result<Value, String> {
    let sock = get_sock(&input);
    let filters = build_prune_filters(&input);
    let path = format!("/v1.43/volumes/prune{}", qs(&[("filters", filters)]));
    let resp = docker_json(host, &sock, "POST", &path, Some(&json!({})))?;
    let deleted: Vec<Value> = resp
        .get("VolumesDeleted")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let space = resp
        .get("SpaceReclaimed")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    Ok(
        json!({ "kind": "volume", "deleted": deleted, "space_reclaimed_bytes": space, "count": deleted.len(), "ok": true }),
    )
}

// ---------------------------------------------------------------------------
// Normalization helpers — map Docker API raw JSON → plugin output shapes.
// ---------------------------------------------------------------------------

fn normalize_container(raw: &Value) -> Value {
    let id = raw
        .get("Id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let names: Vec<String> = raw
        .get("Names")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.trim_start_matches('/').to_string())
                .collect()
        })
        .unwrap_or_default();
    let name = names.first().cloned().unwrap_or_default();
    json!({
        "id": id,
        "short_id": short_id(&id),
        "names": names,
        "name": name,
        "image": raw.get("Image").cloned().unwrap_or(Value::Null),
        "image_id": raw.get("ImageID").cloned().unwrap_or(Value::Null),
        "command": raw.get("Command").cloned().unwrap_or(Value::Null),
        "created": raw.get("Created").cloned().unwrap_or(Value::Null),
        "state": raw.get("State").cloned().unwrap_or(Value::Null),
        "status": raw.get("Status").cloned().unwrap_or(Value::Null),
        "labels": raw.get("Labels").cloned().unwrap_or(json!({})),
    })
}

fn normalize_container_inspect(raw: &Value) -> Value {
    let id = raw
        .get("Id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let name = raw
        .get("Name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim_start_matches('/')
        .to_string();
    let state = raw
        .get("State")
        .and_then(|s| s.get("Status"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    json!({
        "id": id,
        "short_id": short_id(&id),
        "name": name,
        "image": raw.get("Config").and_then(|c| c.get("Image")).cloned().unwrap_or(Value::Null),
        "state": state,
        "created": raw.get("Created").cloned().unwrap_or(Value::Null),
        "labels": raw.get("Config").and_then(|c| c.get("Labels")).cloned().unwrap_or(json!({})),
    })
}

fn normalize_image(raw: &Value) -> Value {
    let id = raw
        .get("Id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    json!({
        "id": id,
        "short_id": short_id(&id),
        "repo_tags": raw.get("RepoTags").cloned().unwrap_or(json!([])),
        "repo_digests": raw.get("RepoDigests").cloned().unwrap_or(json!([])),
        "created": raw.get("Created").cloned().unwrap_or(Value::Null),
        "size": raw.get("Size").cloned().unwrap_or(Value::Null),
        "labels": raw.get("Labels").cloned().unwrap_or(json!({})),
    })
}

fn normalize_image_inspect(raw: &Value) -> Value {
    let id = raw
        .get("Id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    json!({
        "id": id,
        "short_id": short_id(&id),
        "repo_tags": raw.get("RepoTags").cloned().unwrap_or(json!([])),
        "repo_digests": raw.get("RepoDigests").cloned().unwrap_or(json!([])),
        "created": raw.get("Created").cloned().unwrap_or(Value::Null),
        "size": raw.get("Size").cloned().unwrap_or(Value::Null),
        "architecture": raw.get("Architecture").cloned().unwrap_or(Value::Null),
        "os": raw.get("Os").cloned().unwrap_or(Value::Null),
        "docker_version": raw.get("DockerVersion").cloned().unwrap_or(Value::Null),
        "author": raw.get("Author").cloned().unwrap_or(Value::Null),
        "labels": raw.get("Config").and_then(|c| c.get("Labels")).cloned().unwrap_or(json!({})),
    })
}

fn normalize_network(raw: &Value) -> Value {
    let id = raw
        .get("Id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    json!({
        "id": id,
        "short_id": short_id(&id),
        "name": raw.get("Name").cloned().unwrap_or(Value::Null),
        "driver": raw.get("Driver").cloned().unwrap_or(Value::Null),
        "scope": raw.get("Scope").cloned().unwrap_or(Value::Null),
        "internal": raw.get("Internal").cloned().unwrap_or(json!(false)),
        "attachable": raw.get("Attachable").cloned().unwrap_or(json!(false)),
        "labels": raw.get("Labels").cloned().unwrap_or(json!({})),
    })
}

fn normalize_volume(raw: &Value) -> Value {
    json!({
        "name": raw.get("Name").cloned().unwrap_or(Value::Null),
        "driver": raw.get("Driver").cloned().unwrap_or(Value::Null),
        "mountpoint": raw.get("Mountpoint").cloned().unwrap_or(Value::Null),
        "scope": raw.get("Scope").cloned().unwrap_or(Value::Null),
        "created_at": raw.get("CreatedAt").cloned().unwrap_or(Value::Null),
        "labels": raw.get("Labels").cloned().unwrap_or(json!({})),
    })
}

// ---------------------------------------------------------------------------
// Prune filter builder.
// ---------------------------------------------------------------------------

fn build_prune_filters(input: &Value) -> String {
    let mut map = serde_json::Map::new();
    if let Some(until) = opt_str(input, "until") {
        map.insert("until".into(), json!([until]));
    }
    if let Some(labels) = input.get("label").and_then(|v| v.as_array()) {
        if !labels.is_empty() {
            map.insert("label".into(), Value::Array(labels.clone()));
        }
    }
    if map.is_empty() {
        String::new()
    } else {
        enc(&serde_json::to_string(&Value::Object(map)).unwrap_or_default())
    }
}

// ---------------------------------------------------------------------------
// Datasource contribution helpers.
// ---------------------------------------------------------------------------

fn contribute_containers(host: &mut Host, containers: &[Value]) {
    let records: Vec<Record> = containers
        .iter()
        .filter_map(|c| {
            let id = c
                .get("id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())?;
            let name = c.get("name").and_then(|v| v.as_str()).unwrap_or(id);
            Some(Record::new(
                Source::new("docker"),
                "docker.container",
                id,
                name,
                c.to_string(),
            ))
        })
        .collect();
    if !records.is_empty() {
        let _ = host.contribute(&records);
    }
}

fn contribute_images(host: &mut Host, images: &[Value]) {
    let records: Vec<Record> = images
        .iter()
        .filter_map(|img| {
            let id = img
                .get("id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())?;
            let title = img
                .get("repo_tags")
                .and_then(|v| v.as_array())
                .and_then(|a| a.first())
                .and_then(|v| v.as_str())
                .filter(|s| *s != "<none>:<none>")
                .unwrap_or(id);
            Some(Record::new(
                Source::new("docker"),
                "docker.image",
                id,
                title,
                img.to_string(),
            ))
        })
        .collect();
    if !records.is_empty() {
        let _ = host.contribute(&records);
    }
}

fn contribute_networks(host: &mut Host, networks: &[Value]) {
    let records: Vec<Record> = networks
        .iter()
        .filter_map(|n| {
            let id = n
                .get("id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())?;
            let name = n.get("name").and_then(|v| v.as_str()).unwrap_or(id);
            Some(Record::new(
                Source::new("docker"),
                "docker.network",
                id,
                name,
                n.to_string(),
            ))
        })
        .collect();
    if !records.is_empty() {
        let _ = host.contribute(&records);
    }
}

fn contribute_volumes(host: &mut Host, volumes: &[Value]) {
    let records: Vec<Record> = volumes
        .iter()
        .filter_map(|v| {
            let name = v
                .get("name")
                .and_then(|val| val.as_str())
                .filter(|s| !s.is_empty())?;
            Some(Record::new(
                Source::new("docker"),
                "docker.volume",
                name,
                name,
                v.to_string(),
            ))
        })
        .collect();
    if !records.is_empty() {
        let _ = host.contribute(&records);
    }
}

// ---------------------------------------------------------------------------
// Entry point.
// ---------------------------------------------------------------------------

fn main() {
    manifest_builder().serve();
}

// ---------------------------------------------------------------------------
// Tests — one MockHost test per op using with_conn_response.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn http_200(body: &str) -> Vec<u8> {
        let body_bytes = body.as_bytes();
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body_bytes.len(),
            body
        )
        .into_bytes()
    }

    fn http_204() -> Vec<u8> {
        b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n".to_vec()
    }

    fn http_201(body: &str) -> Vec<u8> {
        let body_bytes = body.as_bytes();
        format!(
            "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body_bytes.len(),
            body
        )
        .into_bytes()
    }

    fn plugin() -> Plugin {
        manifest_builder().build()
    }

    #[test]
    fn test_system_info() {
        let info_body = r#"{"ID":"abc","Name":"docker-desktop","ServerVersion":"24.0.5","OSType":"linux","OperatingSystem":"Docker Desktop","Architecture":"x86_64","KernelVersion":"5.15.0","Containers":3,"ContainersRunning":1,"ContainersPaused":0,"ContainersStopped":2,"Images":5,"NCPU":4,"MemTotal":8000000000,"DockerRootDir":"/var/lib/docker","Warnings":[]}"#;
        let mut host = MockHost::default().with_conn_response(http_200(info_body));
        let out = plugin().call("docker.info", json!({}), &mut host).unwrap();
        assert_eq!(out["name"], "docker-desktop");
        assert_eq!(out["containers"], 3);
    }

    #[test]
    fn test_system_df() {
        let df_body = r#"{"Images":[],"Containers":[],"Volumes":[],"BuildCache":[]}"#;
        let mut host = MockHost::default().with_conn_response(http_200(df_body));
        let out = plugin()
            .call("docker.system.df", json!({}), &mut host)
            .unwrap();
        assert!(out.get("Images").is_some());
    }

    #[test]
    fn test_container_list() {
        let body = r#"[{"Id":"abc123def456","Names":["/mycontainer"],"Image":"nginx:alpine","ImageID":"sha256:aabbcc","Command":"nginx","Created":1234567890,"State":"running","Status":"Up 2 hours","Labels":{}}]"#;
        let mut host = MockHost::default().with_conn_response(http_200(body));
        let out = plugin()
            .call("docker.container.list", json!({"all": true}), &mut host)
            .unwrap();
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"], "mycontainer");
        assert_eq!(host.contributed.borrow().len(), 1);
    }

    #[test]
    fn test_container_show() {
        let body = r#"{"Id":"abc123def456","Name":"/mycontainer","Config":{"Image":"nginx:alpine","Labels":{}},"State":{"Status":"running"},"Created":"2024-01-01T00:00:00Z"}"#;
        let mut host = MockHost::default().with_conn_response(http_200(body));
        let out = plugin()
            .call("docker.container.show", json!({"id": "abc123"}), &mut host)
            .unwrap();
        assert_eq!(out["name"], "mycontainer");
        assert_eq!(out["state"], "running");
    }

    #[test]
    fn test_container_logs() {
        // Docker multiplexed log: 8-byte header (type=1/stdout, size=12) + "Hello World\n"
        let payload = b"Hello World\n";
        let mut frame = vec![0x01u8, 0x00, 0x00, 0x00];
        let len = payload.len() as u32;
        frame.extend_from_slice(&len.to_be_bytes());
        frame.extend_from_slice(payload);
        let mut resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\n\r\n",
            frame.len()
        ).into_bytes();
        resp.extend_from_slice(&frame);
        let mut host = MockHost::default().with_conn_response(resp);
        let out = plugin()
            .call("docker.container.logs", json!({"id": "abc123"}), &mut host)
            .unwrap();
        assert!(out["text"].as_str().unwrap().contains("Hello World"));
    }

    #[test]
    fn test_container_top() {
        let body = r#"{"Titles":["PID","USER","COMMAND"],"Processes":[["1","root","nginx"],["12","nginx","nginx"]]}"#;
        let mut host = MockHost::default().with_conn_response(http_200(body));
        let out = plugin()
            .call("docker.container.top", json!({"id": "abc123"}), &mut host)
            .unwrap();
        assert_eq!(out["count"], 2);
        let titles = out["titles"].as_array().unwrap();
        assert_eq!(titles[0], "PID");
    }

    #[test]
    fn test_container_inspect_raw() {
        let body = r#"{"Id":"abc123def456","Name":"/mycontainer","Config":{"Image":"nginx"},"State":{"Status":"running"}}"#;
        let mut host = MockHost::default().with_conn_response(http_200(body));
        let out = plugin()
            .call(
                "docker.container.inspect.raw",
                json!({"id": "abc123"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["kind"], "container");
        assert!(out["data"].is_object());
    }

    #[test]
    fn test_container_start() {
        let mut host = MockHost::default().with_conn_response(http_204());
        let out = plugin()
            .call("docker.container.start", json!({"id": "abc123"}), &mut host)
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["action"], "start");
    }

    #[test]
    fn test_container_stop() {
        let mut host = MockHost::default().with_conn_response(http_204());
        let out = plugin()
            .call("docker.container.stop", json!({"id": "abc123"}), &mut host)
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["action"], "stop");
    }

    #[test]
    fn test_container_restart() {
        let mut host = MockHost::default().with_conn_response(http_204());
        let out = plugin()
            .call(
                "docker.container.restart",
                json!({"id": "abc123"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
    }

    #[test]
    fn test_container_remove() {
        let mut host = MockHost::default().with_conn_response(http_204());
        let out = plugin()
            .call(
                "docker.container.remove",
                json!({"id": "abc123"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
    }

    #[test]
    fn test_container_create() {
        let body = r#"{"Id":"newcontainer123","Warnings":[]}"#;
        let mut host = MockHost::default().with_conn_response(http_201(body));
        let out = plugin()
            .call(
                "docker.container.create",
                json!({"image": "nginx:alpine", "name": "web"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["id"], "newcontainer123");
    }

    #[test]
    fn test_container_run() {
        let create_body = r#"{"Id":"newcontainer456","Warnings":[]}"#;
        let mut host = MockHost::default()
            .with_conn_response(http_201(create_body))
            .with_conn_response(http_204());
        let out = plugin()
            .call(
                "docker.container.run",
                json!({"image": "nginx:alpine", "name": "web2"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["started"], true);
    }

    #[test]
    fn test_container_prune() {
        let body = r#"{"ContainersDeleted":["abc","def"],"SpaceReclaimed":1024}"#;
        let mut host = MockHost::default().with_conn_response(http_200(body));
        let out = plugin()
            .call("docker.container.prune", json!({}), &mut host)
            .unwrap();
        assert_eq!(out["count"], 2);
        assert_eq!(out["space_reclaimed_bytes"], 1024);
    }

    #[test]
    fn test_image_list() {
        let body = r#"[{"Id":"sha256:aabbccdd11223344","RepoTags":["nginx:alpine"],"RepoDigests":[],"Created":1234567890,"Size":20000000,"Labels":{}}]"#;
        let mut host = MockHost::default().with_conn_response(http_200(body));
        let out = plugin()
            .call("docker.image.list", json!({}), &mut host)
            .unwrap();
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["repo_tags"][0], "nginx:alpine");
        assert_eq!(host.contributed.borrow().len(), 1);
    }

    #[test]
    fn test_image_show() {
        let body = r#"{"Id":"sha256:aabbccdd11223344","RepoTags":["nginx:alpine"],"RepoDigests":[],"Created":"2024-01-01T00:00:00Z","Size":20000000,"Architecture":"amd64","Os":"linux","DockerVersion":"24.0","Author":"","Config":{"Labels":{}}}"#;
        let mut host = MockHost::default().with_conn_response(http_200(body));
        let out = plugin()
            .call(
                "docker.image.show",
                json!({"id": "nginx:alpine"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["repo_tags"][0], "nginx:alpine");
        assert_eq!(out["architecture"], "amd64");
    }

    #[test]
    fn test_image_inspect_raw() {
        let body = r#"{"Id":"sha256:aabb","RepoTags":["nginx:alpine"],"Config":{}}"#;
        let mut host = MockHost::default().with_conn_response(http_200(body));
        let out = plugin()
            .call(
                "docker.image.inspect.raw",
                json!({"id": "nginx:alpine"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["kind"], "image");
        assert!(out["data"].is_object());
    }

    #[test]
    fn test_image_pull() {
        let body = "{\"status\":\"Pulling from library/nginx\"}\n{\"status\":\"Pull complete\"}\n";
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        )
        .into_bytes();
        let mut host = MockHost::default().with_conn_response(resp);
        let out = plugin()
            .call(
                "docker.image.pull",
                json!({"reference": "nginx:alpine"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["reference"], "nginx:alpine");
    }

    #[test]
    fn test_image_tag() {
        let mut host = MockHost::default().with_conn_response(http_204());
        let out = plugin()
            .call(
                "docker.image.tag",
                json!({"source": "nginx:alpine", "target": "myregistry/nginx:1.0"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
    }

    #[test]
    fn test_image_remove() {
        let body = r#"[{"Untagged":"nginx:alpine"},{"Deleted":"sha256:aabb"}]"#;
        let mut host = MockHost::default().with_conn_response(http_200(body));
        let out = plugin()
            .call(
                "docker.image.remove",
                json!({"id": "nginx:alpine"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
        let deleted = out["deleted"].as_array().unwrap();
        assert_eq!(deleted.len(), 2);
    }

    #[test]
    fn test_image_prune() {
        let body = r#"{"ImagesDeleted":[{"Untagged":"old:latest"}],"SpaceReclaimed":5000}"#;
        let mut host = MockHost::default().with_conn_response(http_200(body));
        let out = plugin()
            .call("docker.image.prune", json!({}), &mut host)
            .unwrap();
        assert_eq!(out["count"], 1);
        assert_eq!(out["space_reclaimed_bytes"], 5000);
    }

    #[test]
    fn test_network_list() {
        let body = r#"[{"Id":"net1id","Name":"bridge","Driver":"bridge","Scope":"local","Internal":false,"Attachable":false,"Labels":{}}]"#;
        let mut host = MockHost::default().with_conn_response(http_200(body));
        let out = plugin()
            .call("docker.network.list", json!({}), &mut host)
            .unwrap();
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"], "bridge");
        assert_eq!(host.contributed.borrow().len(), 1);
    }

    #[test]
    fn test_network_show() {
        let body = r#"{"Id":"net1id","Name":"bridge","Driver":"bridge","Scope":"local","Internal":false,"Attachable":false,"Labels":{}}"#;
        let mut host = MockHost::default().with_conn_response(http_200(body));
        let out = plugin()
            .call("docker.network.show", json!({"id": "bridge"}), &mut host)
            .unwrap();
        assert_eq!(out["name"], "bridge");
    }

    #[test]
    fn test_network_inspect_raw() {
        let body = r#"{"Id":"net1id","Name":"bridge","Driver":"bridge"}"#;
        let mut host = MockHost::default().with_conn_response(http_200(body));
        let out = plugin()
            .call(
                "docker.network.inspect.raw",
                json!({"id": "bridge"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["kind"], "network");
    }

    #[test]
    fn test_network_create() {
        let body = r#"{"Id":"newnetid123"}"#;
        let mut host = MockHost::default().with_conn_response(http_201(body));
        let out = plugin()
            .call(
                "docker.network.create",
                json!({"name": "mynet", "driver": "bridge"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["ok"], true);
        assert_eq!(out["id"], "newnetid123");
    }

    #[test]
    fn test_network_remove() {
        let mut host = MockHost::default().with_conn_response(http_204());
        let out = plugin()
            .call("docker.network.remove", json!({"id": "mynet"}), &mut host)
            .unwrap();
        assert_eq!(out["ok"], true);
    }

    #[test]
    fn test_network_prune() {
        let body = r#"{"NetworksDeleted":["myoldnet"]}"#;
        let mut host = MockHost::default().with_conn_response(http_200(body));
        let out = plugin()
            .call("docker.network.prune", json!({}), &mut host)
            .unwrap();
        assert_eq!(out["count"], 1);
    }

    #[test]
    fn test_volume_list() {
        let body = r#"{"Volumes":[{"Name":"myvol","Driver":"local","Mountpoint":"/var/lib/docker/volumes/myvol/_data","Scope":"local","CreatedAt":"2024-01-01T00:00:00Z","Labels":{}}],"Warnings":[]}"#;
        let mut host = MockHost::default().with_conn_response(http_200(body));
        let out = plugin()
            .call("docker.volume.list", json!({}), &mut host)
            .unwrap();
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"], "myvol");
        assert_eq!(host.contributed.borrow().len(), 1);
    }

    #[test]
    fn test_volume_show() {
        let body = r#"{"Name":"myvol","Driver":"local","Mountpoint":"/var/lib/docker/volumes/myvol/_data","Scope":"local","Labels":{}}"#;
        let mut host = MockHost::default().with_conn_response(http_200(body));
        let out = plugin()
            .call("docker.volume.show", json!({"id": "myvol"}), &mut host)
            .unwrap();
        assert_eq!(out["name"], "myvol");
    }

    #[test]
    fn test_volume_inspect_raw() {
        let body = r#"{"Name":"myvol","Driver":"local","Mountpoint":"/data"}"#;
        let mut host = MockHost::default().with_conn_response(http_200(body));
        let out = plugin()
            .call(
                "docker.volume.inspect.raw",
                json!({"id": "myvol"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["kind"], "volume");
    }

    #[test]
    fn test_volume_create() {
        let body = r#"{"Name":"newvol","Driver":"local","Mountpoint":"/var/lib/docker/volumes/newvol/_data","Scope":"local","Labels":{}}"#;
        let mut host = MockHost::default().with_conn_response(http_201(body));
        let out = plugin()
            .call(
                "docker.volume.create",
                json!({"name": "newvol", "driver": "local"}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["name"], "newvol");
    }

    #[test]
    fn test_volume_remove() {
        let mut host = MockHost::default().with_conn_response(http_204());
        let out = plugin()
            .call("docker.volume.remove", json!({"id": "myvol"}), &mut host)
            .unwrap();
        assert_eq!(out["ok"], true);
    }

    #[test]
    fn test_volume_prune() {
        let body = r#"{"VolumesDeleted":["oldvol1","oldvol2"],"SpaceReclaimed":2048}"#;
        let mut host = MockHost::default().with_conn_response(http_200(body));
        let out = plugin()
            .call("docker.volume.prune", json!({}), &mut host)
            .unwrap();
        assert_eq!(out["count"], 2);
        assert_eq!(out["space_reclaimed_bytes"], 2048);
    }

    // -----------------------------------------------------------------------
    // D-36 real-gap tests (failing-first): params/behaviour present in
    // fluxplane Go but missing from the flux docker plugin before this pass.
    // -----------------------------------------------------------------------

    #[test]
    fn test_system_df_types_filter() {
        let body = r#"{"Images":[],"Containers":[],"Volumes":[],"BuildCache":[]}"#;
        let mut host = MockHost::default().with_conn_response(http_200(body));
        plugin()
            .call(
                "docker.system.df",
                json!({"types": ["image", "container"]}),
                &mut host,
            )
            .unwrap();
        let buf = host.conn_buf.borrow();
        let requests = String::from_utf8_lossy(&buf);
        assert!(requests.contains("type=%7B"), "got {requests}");
        assert!(requests.contains("%22image%22%3Atrue"), "got {requests}");
        assert!(
            requests.contains("%22container%22%3Atrue"),
            "got {requests}"
        );
    }

    #[test]
    fn test_container_top_args_array() {
        let body = r#"{"Titles":["PID","COMMAND"],"Processes":[["1","nginx"]]}"#;
        let mut host = MockHost::default().with_conn_response(http_200(body));
        plugin()
            .call(
                "docker.container.top",
                json!({"id": "abc123", "args": ["-e", "-f"]}),
                &mut host,
            )
            .unwrap();
        let buf = host.conn_buf.borrow();
        let requests = String::from_utf8_lossy(&buf);
        assert!(requests.contains("ps_args=-e%20-f"), "got {requests}");
    }

    #[test]
    fn test_container_restart_signal() {
        let mut host = MockHost::default().with_conn_response(http_204());
        plugin()
            .call(
                "docker.container.restart",
                json!({"id": "abc123", "signal": "SIGKILL"}),
                &mut host,
            )
            .unwrap();
        let buf = host.conn_buf.borrow();
        let requests = String::from_utf8_lossy(&buf);
        assert!(requests.contains("signal=SIGKILL"), "got {requests}");
    }

    #[test]
    fn test_container_create_mounts_and_open_stdin() {
        let body = r#"{"Id":"new","Warnings":[]}"#;
        let mut host = MockHost::default().with_conn_response(http_201(body));
        plugin()
            .call(
                "docker.container.create",
                json!({
                    "image": "nginx:alpine",
                    "name": "dev",
                    "open_stdin": true,
                    "mounts": [{"type": "bind", "source": "/host", "target": "/container", "read_only": true}]
                }),
                &mut host,
            )
            .unwrap();
        let buf = host.conn_buf.borrow();
        let requests = String::from_utf8_lossy(&buf);
        assert!(requests.contains(r#""OpenStdin":true"#), "got {requests}");
        assert!(requests.contains(r#""Mounts":["#), "got {requests}");
        assert!(requests.contains(r#""ReadOnly":true"#), "got {requests}");
    }

    #[test]
    fn test_network_create_extra_fields() {
        let body = r#"{"Id":"net123"}"#;
        let mut host = MockHost::default().with_conn_response(http_201(body));
        plugin()
            .call(
                "docker.network.create",
                json!({
                    "name": "mynet",
                    "scope": "global",
                    "ingress": true,
                    "enable_ipv6": true
                }),
                &mut host,
            )
            .unwrap();
        let buf = host.conn_buf.borrow();
        let requests = String::from_utf8_lossy(&buf);
        assert!(requests.contains(r#""Scope":"global""#), "got {requests}");
        assert!(requests.contains(r#""Ingress":true"#), "got {requests}");
        assert!(requests.contains(r#""EnableIPv6":true"#), "got {requests}");
    }

    #[test]
    fn test_network_list_limit() {
        let body = r#"[{"Id":"n1","Name":"net1","Driver":"bridge","Scope":"local","Internal":false,"Attachable":false,"Labels":{}},{"Id":"n2","Name":"net2","Driver":"bridge","Scope":"local","Internal":false,"Attachable":false,"Labels":{}}]"#;
        let mut host = MockHost::default().with_conn_response(http_200(body));
        let out = plugin()
            .call("docker.network.list", json!({"limit": 1}), &mut host)
            .unwrap();
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"], "net1");
    }

    #[test]
    fn test_volume_list_limit() {
        let body = r#"{"Volumes":[{"Name":"v1","Driver":"local","Scope":"local","Labels":{}},{"Name":"v2","Driver":"local","Scope":"local","Labels":{}}],"Warnings":[]}"#;
        let mut host = MockHost::default().with_conn_response(http_200(body));
        let out = plugin()
            .call("docker.volume.list", json!({"limit": 1}), &mut host)
            .unwrap();
        let arr = out.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"], "v1");
    }

    #[test]
    fn test_image_pull_limit() {
        let body = "{\"status\":\"a\"}\n{\"status\":\"b\"}\n{\"status\":\"c\"}\n";
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        )
        .into_bytes();
        let mut host = MockHost::default().with_conn_response(resp);
        let out = plugin()
            .call(
                "docker.image.pull",
                json!({"reference": "nginx", "limit": 2}),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["count"], 2);
    }
}

// ===========================================================================
// D-36: schema-derivation contract test.
// Locks each op's derived schemars schema to its intended field/required/type
// contract (encoded from the struct definitions). A change here is a real
// contract change.
// ===========================================================================
#[cfg(test)]
mod schema_contract {
    use super::*;
    use std::collections::BTreeMap;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Kind {
        Str,
        Int,
        Bool,
        ArrayStr,
        ArrayAny,
        Object,
    }

    #[derive(Clone)]
    struct Prop {
        name: &'static str,
        kind: Kind,
    }

    #[derive(Clone)]
    struct OpContract {
        props: Vec<Prop>,
        required: Vec<&'static str>,
    }

    fn p(name: &'static str, kind: Kind) -> Prop {
        Prop { name, kind }
    }

    fn c(props: Vec<Prop>, required: Vec<&'static str>) -> OpContract {
        OpContract { props, required }
    }

    fn socket_prop() -> Prop {
        p("socket", Kind::Str)
    }

    fn container_create_contract() -> OpContract {
        c(
            vec![
                socket_prop(),
                p("image", Kind::Str),
                p("name", Kind::Str),
                p("cmd", Kind::ArrayStr),
                p("entrypoint", Kind::ArrayStr),
                p("env", Kind::ArrayStr),
                p("labels", Kind::Object),
                p("workdir", Kind::Str),
                p("user", Kind::Str),
                p("hostname", Kind::Str),
                p("network", Kind::Str),
                p("restart", Kind::Str),
                p("auto_remove", Kind::Bool),
                p("tty", Kind::Bool),
                p("open_stdin", Kind::Bool),
                p("privileged", Kind::Bool),
                p("binds", Kind::ArrayStr),
                p("mounts", Kind::ArrayAny),
                p("ports", Kind::ArrayAny),
                p("platform", Kind::Str),
            ],
            vec!["image"],
        )
    }

    fn contracts() -> Vec<(&'static str, OpContract)> {
        vec![
            ("docker.info", c(vec![socket_prop()], vec![])),
            (
                "docker.system.df",
                c(vec![socket_prop(), p("types", Kind::ArrayStr)], vec![]),
            ),
            (
                "docker.container.list",
                c(
                    vec![
                        socket_prop(),
                        p("all", Kind::Bool),
                        p("limit", Kind::Int),
                        p("status", Kind::ArrayStr),
                        p("name", Kind::ArrayStr),
                        p("label", Kind::ArrayStr),
                    ],
                    vec![],
                ),
            ),
            (
                "docker.container.show",
                c(vec![socket_prop(), p("id", Kind::Str)], vec!["id"]),
            ),
            (
                "docker.container.logs",
                c(
                    vec![
                        socket_prop(),
                        p("id", Kind::Str),
                        p("tail", Kind::Int),
                        p("since", Kind::Str),
                        p("until", Kind::Str),
                        p("timestamps", Kind::Bool),
                    ],
                    vec!["id"],
                ),
            ),
            (
                "docker.container.top",
                c(
                    vec![socket_prop(), p("id", Kind::Str), p("args", Kind::ArrayStr)],
                    vec!["id"],
                ),
            ),
            (
                "docker.container.inspect.raw",
                c(vec![socket_prop(), p("id", Kind::Str)], vec!["id"]),
            ),
            (
                "docker.container.start",
                c(vec![socket_prop(), p("id", Kind::Str)], vec!["id"]),
            ),
            (
                "docker.container.stop",
                c(
                    vec![
                        socket_prop(),
                        p("id", Kind::Str),
                        p("timeout", Kind::Int),
                        p("signal", Kind::Str),
                    ],
                    vec!["id"],
                ),
            ),
            (
                "docker.container.restart",
                c(
                    vec![
                        socket_prop(),
                        p("id", Kind::Str),
                        p("timeout", Kind::Int),
                        p("signal", Kind::Str),
                    ],
                    vec!["id"],
                ),
            ),
            (
                "docker.container.remove",
                c(
                    vec![
                        socket_prop(),
                        p("id", Kind::Str),
                        p("force", Kind::Bool),
                        p("volumes", Kind::Bool),
                    ],
                    vec!["id"],
                ),
            ),
            ("docker.container.create", container_create_contract()),
            ("docker.container.run", container_create_contract()),
            (
                "docker.container.prune",
                c(
                    vec![
                        socket_prop(),
                        p("until", Kind::Str),
                        p("label", Kind::ArrayStr),
                    ],
                    vec![],
                ),
            ),
            (
                "docker.image.list",
                c(
                    vec![
                        socket_prop(),
                        p("all", Kind::Bool),
                        p("limit", Kind::Int),
                        p("reference", Kind::ArrayStr),
                        p("label", Kind::ArrayStr),
                    ],
                    vec![],
                ),
            ),
            (
                "docker.image.show",
                c(vec![socket_prop(), p("id", Kind::Str)], vec!["id"]),
            ),
            (
                "docker.image.inspect.raw",
                c(vec![socket_prop(), p("id", Kind::Str)], vec!["id"]),
            ),
            (
                "docker.image.pull",
                c(
                    vec![
                        socket_prop(),
                        p("reference", Kind::Str),
                        p("platform", Kind::Str),
                        p("limit", Kind::Int),
                    ],
                    vec!["reference"],
                ),
            ),
            (
                "docker.image.tag",
                c(
                    vec![
                        socket_prop(),
                        p("source", Kind::Str),
                        p("target", Kind::Str),
                    ],
                    vec!["source", "target"],
                ),
            ),
            (
                "docker.image.remove",
                c(
                    vec![
                        socket_prop(),
                        p("id", Kind::Str),
                        p("force", Kind::Bool),
                        p("noprune", Kind::Bool),
                    ],
                    vec!["id"],
                ),
            ),
            (
                "docker.image.prune",
                c(
                    vec![
                        socket_prop(),
                        p("all", Kind::Bool),
                        p("until", Kind::Str),
                        p("label", Kind::ArrayStr),
                    ],
                    vec![],
                ),
            ),
            (
                "docker.network.list",
                c(
                    vec![
                        socket_prop(),
                        p("limit", Kind::Int),
                        p("name", Kind::ArrayStr),
                        p("label", Kind::ArrayStr),
                    ],
                    vec![],
                ),
            ),
            (
                "docker.network.show",
                c(vec![socket_prop(), p("id", Kind::Str)], vec!["id"]),
            ),
            (
                "docker.network.inspect.raw",
                c(vec![socket_prop(), p("id", Kind::Str)], vec!["id"]),
            ),
            (
                "docker.network.create",
                c(
                    vec![
                        socket_prop(),
                        p("name", Kind::Str),
                        p("driver", Kind::Str),
                        p("scope", Kind::Str),
                        p("internal", Kind::Bool),
                        p("attachable", Kind::Bool),
                        p("ingress", Kind::Bool),
                        p("enable_ipv4", Kind::Bool),
                        p("enable_ipv6", Kind::Bool),
                        p("options", Kind::Object),
                        p("labels", Kind::Object),
                    ],
                    vec!["name"],
                ),
            ),
            (
                "docker.network.remove",
                c(vec![socket_prop(), p("id", Kind::Str)], vec!["id"]),
            ),
            (
                "docker.network.prune",
                c(
                    vec![
                        socket_prop(),
                        p("until", Kind::Str),
                        p("label", Kind::ArrayStr),
                    ],
                    vec![],
                ),
            ),
            (
                "docker.volume.list",
                c(
                    vec![
                        socket_prop(),
                        p("limit", Kind::Int),
                        p("name", Kind::ArrayStr),
                        p("label", Kind::ArrayStr),
                    ],
                    vec![],
                ),
            ),
            (
                "docker.volume.show",
                c(vec![socket_prop(), p("id", Kind::Str)], vec!["id"]),
            ),
            (
                "docker.volume.inspect.raw",
                c(vec![socket_prop(), p("id", Kind::Str)], vec!["id"]),
            ),
            (
                "docker.volume.create",
                c(
                    vec![
                        socket_prop(),
                        p("name", Kind::Str),
                        p("driver", Kind::Str),
                        p("driver_opts", Kind::Object),
                        p("labels", Kind::Object),
                    ],
                    vec![],
                ),
            ),
            (
                "docker.volume.remove",
                c(
                    vec![socket_prop(), p("id", Kind::Str), p("force", Kind::Bool)],
                    vec!["id"],
                ),
            ),
            (
                "docker.volume.prune",
                c(
                    vec![
                        socket_prop(),
                        p("until", Kind::Str),
                        p("label", Kind::ArrayStr),
                    ],
                    vec![],
                ),
            ),
        ]
    }

    fn resolve<'a>(node: &'a Value, defs: &'a Value) -> &'a Value {
        if let Some(obj) = node.as_object() {
            if let Some(r) = obj.get("$ref").and_then(|v| v.as_str()) {
                if let Some(name) = r.strip_prefix("#/definitions/") {
                    return defs.get(name).unwrap_or(node);
                }
            }
            if let Some(any) = obj.get("anyOf").and_then(|v| v.as_array()) {
                for m in any {
                    if m.get("type").and_then(|v| v.as_str()) != Some("null") {
                        return resolve(m, defs);
                    }
                }
            }
        }
        node
    }

    fn kind_of(node: &Value) -> Kind {
        let t = node.get("type");
        if let Some(arr) = t.and_then(|v| v.as_array()) {
            let first = arr
                .iter()
                .find(|v| v.as_str() != Some("null"))
                .and_then(|v| v.as_str())
                .unwrap_or("null");
            return base_kind(first, node);
        }
        base_kind(t.and_then(|v| v.as_str()).unwrap_or(""), node)
    }

    fn base_kind(t: &str, node: &Value) -> Kind {
        match t {
            "integer" => Kind::Int,
            "boolean" => Kind::Bool,
            "string" => Kind::Str,
            "array" => {
                let items = node.get("items").cloned().unwrap_or(Value::Null);
                if items.get("type").and_then(|v| v.as_str()) == Some("string") {
                    Kind::ArrayStr
                } else {
                    Kind::ArrayAny
                }
            }
            "object" | "" => Kind::Object,
            other => panic!("unsupported property type: {other} ({node})"),
        }
    }

    fn assert_contract(op_name: &str, schema: &Value, contract: &OpContract) {
        let defs = schema.get("definitions").cloned().unwrap_or(json!({}));
        assert_eq!(schema["type"], "object", "{op_name}: root type");
        let props_obj = schema.get("properties").and_then(|v| v.as_object());
        let mut got: BTreeMap<&str, Kind> = BTreeMap::new();
        if let Some(props) = props_obj {
            for (k, v) in props {
                let resolved = resolve(v, &defs);
                got.insert(k.as_str(), kind_of(resolved));
            }
        }
        let want: BTreeMap<&str, Kind> = contract
            .props
            .iter()
            .map(|Prop { name, kind }| (*name, kind.clone()))
            .collect();
        assert_eq!(got.len(), want.len(), "{op_name}: property count");
        for Prop { name, kind } in &contract.props {
            let got_kind = got
                .get(*name)
                .unwrap_or_else(|| panic!("{op_name}: missing property `{name}`"));
            assert_eq!(got_kind, kind, "{op_name}: property `{name}` kind");
        }
        let mut req: Vec<&str> = schema
            .get("required")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        req.sort();
        let mut want_req: Vec<&str> = contract.required.clone();
        want_req.sort();
        assert_eq!(req, want_req, "{op_name}: required set");
    }

    #[test]
    fn derived_schemas_match_contract() {
        let ops = contracts();
        let manifest = manifest_builder().build().manifest();
        let by_name: BTreeMap<&str, &OperationSpec> = manifest
            .operations
            .iter()
            .map(|o| (o.name.as_str(), o))
            .collect();
        assert_eq!(by_name.len(), ops.len(), "op count changed");
        for (name, contract) in &ops {
            let spec = by_name
                .get(*name)
                .unwrap_or_else(|| panic!("missing op {name}"));
            assert_contract(name, &spec.input_schema, contract);
        }
    }
}
