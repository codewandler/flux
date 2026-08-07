//! Authenticated HTTPS transport for the guarded execution-system port.
//!
//! The local runtime still authorizes and approves an effect before it reaches this module. The
//! daemon executes the literal request through its own native [`System`], so workspace confinement,
//! process construction and other physical guarantees are enforced where the effect lands.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::ws::{Message as ServerWsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, Path as AxumPath, State};
use axum::http::StatusCode;
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use flux_core::{Error, GuardedIoError, GuardedIoFailure, Result};
use flux_system::metrics::{
    bounded_label, CpuUsage, FanSensor, LoadAverage, MemoryUsage, MetricAnswer, MetricKind,
    MetricReading, MetricSnapshot, MetricUnavailable, MountUsage, TemperatureSensor, MAX_MOUNTS,
    MAX_SENSORS,
};
use flux_system::net::{
    BindExposure, DatagramEndpoint, DatagramHandle, DialTarget, DuplexReadHalf,
    DuplexStream as GuardedDuplex, DuplexWriteHalf, InboundLimits, NetworkListener, NetworkStream,
    PrivateNetAllow, StreamListener,
};
use flux_system::port::{ExecutionIdentity, GuardedMetrics, GuardedNetwork, SubstrateIdentity};
use flux_system::remote::{Answer, Answered, Delegate, Delivered, RemoteSystem, Unreachable};
use flux_system::{
    ChildStatus, ManagedChild, ManagedProcess, ProcessOutput, ScopedFileRead, System,
};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tower_http::timeout::TimeoutLayer;

use crate::{guard_open_bind, require_auth, ServerAuth};

/// Version of the remote execution-system wire contract.
///
/// v3 (C-654) added the bounded `host.metrics` operation and the handshake's `metric_kinds`
/// declaration. Decision 0018 rule 5 fixes the rule this follows: wire support for a new port
/// family is a *versioned* protocol change, never an implicit extension — so the version moves and
/// a mixed pair refuses to pair at all, rather than discovering the gap one operation at a time.
pub const PROTOCOL_VERSION: u32 = 3;
const MAX_REQUEST_BYTES: usize = 2 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
static CLIENT_INSTANCE_SEQ: AtomicU64 = AtomicU64::new(1);

/// Handshake returned before an execution system is installed into a local turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemHandshake {
    pub protocol_version: u32,
    pub substrate_kind: String,
    pub workspace: String,
    pub confinement: String,
    pub operations: Vec<String>,
    /// Which [`MetricKind`](flux_system::metrics::MetricKind) tokens the serving substrate declares
    /// it can measure about itself (C-654).
    ///
    /// A *capability declaration*, alongside `operations` and for the same reason: it makes
    /// `served_metric_kinds` free and honest instead of a round trip that a caller would be tempted
    /// to cache. Defaulted because a peer that declares nothing measures nothing — which is the
    /// port's `Unserved`, and a different answer from a machine whose instruments are missing.
    #[serde(default)]
    pub metric_kinds: Vec<String>,
}

impl SystemHandshake {
    /// The substrate identity this handshake establishes. Public because the ssh binding (C-683)
    /// composes the same client from one layer out and must install the same identity a directly
    /// addressed `remote` binding would — `remotely_reported` included.
    pub fn identity(&self) -> SubstrateIdentity {
        SubstrateIdentity {
            kind: self.substrate_kind.clone(),
            workspace: self.workspace.clone(),
            confinement: self.confinement.clone(),
            remotely_reported: true,
        }
    }

    /// The declared kinds, resolved against the **closed** local vocabulary.
    ///
    /// Never trust the wire: a token this build does not know is dropped rather than guessed at,
    /// and the result is capped by the vocabulary itself, so a peer cannot enlarge what a caller
    /// will iterate over by repeating or inventing tokens.
    fn declared_metric_kinds(&self) -> Vec<MetricKind> {
        let mut kinds: Vec<MetricKind> = MetricKind::ALL
            .into_iter()
            .filter(|kind| self.metric_kinds.iter().any(|token| token == kind.as_str()))
            .collect();
        kinds.dedup();
        kinds
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WireRequest {
    protocol_version: u32,
    operation_id: String,
    fingerprint: String,
    operation: String,
    arguments: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WireAnswer {
    status: WireStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    value: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireStatus {
    Served,
    Refused,
    Unserved,
    Unknown,
}

#[derive(Clone)]
struct SystemState {
    system: Arc<System>,
    handshake: SystemHandshake,
    delivery: Arc<DeliveryLedger>,
    network: Arc<NetworkResources>,
}

#[derive(Default)]
struct NetworkResources {
    next_id: AtomicU64,
    streams: tokio::sync::Mutex<HashMap<String, PendingStream>>,
}

struct PendingStream {
    stream: NetworkStream,
    expires_at: Instant,
}

const DELIVERY_LEDGER_PATH: &str = ".flux/remote-system-delivery.json";
const MAX_DELIVERY_RECORDS: usize = 4096;
const MAX_DELIVERY_LEDGER_BYTES: usize = 1024 * 1024;
const DELIVERY_RETENTION_MS: u64 = 7 * 24 * 60 * 60 * 1000;
const MAX_PENDING_STREAMS: usize = 1024;
const PENDING_STREAM_TTL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeliveryRecord {
    operation_id: String,
    fingerprint: String,
    state: DeliveryState,
    accepted_at_ms: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum DeliveryState {
    Accepted,
    Completed,
    Unknown,
}

#[derive(Default)]
struct DeliveryData {
    records: Vec<DeliveryRecord>,
    cached: HashMap<String, WireAnswer>,
}

struct DeliveryLedger {
    system: Arc<System>,
    data: tokio::sync::Mutex<Option<DeliveryData>>,
}

enum DeliveryClaim {
    Execute,
    Cached(WireAnswer),
    Refused(String),
    Unknown(String),
}

impl DeliveryLedger {
    fn new(system: Arc<System>) -> Self {
        Self {
            system,
            data: tokio::sync::Mutex::new(None),
        }
    }

    async fn claim(&self, request: &WireRequest) -> Result<DeliveryClaim> {
        if request.operation_id.trim().is_empty() || request.operation_id.len() > 128 {
            return Ok(DeliveryClaim::Refused(
                "operation id must contain 1–128 bytes".into(),
            ));
        }
        if request.fingerprint.len() > 128 || request.operation.len() > 128 {
            return Ok(DeliveryClaim::Refused(
                "operation name and fingerprint must each fit in 128 bytes".into(),
            ));
        }
        let mut slot = self.data.lock().await;
        self.load_if_needed(&mut slot).await?;
        let data = slot.as_mut().expect("delivery ledger initialized");
        let cutoff = now_millis()?.saturating_sub(DELIVERY_RETENTION_MS);
        data.records.retain(|record| {
            record.state == DeliveryState::Accepted || record.accepted_at_ms >= cutoff
        });
        data.cached
            .retain(|id, _| data.records.iter().any(|record| record.operation_id == *id));
        if let Some(record) = data
            .records
            .iter()
            .find(|record| record.operation_id == request.operation_id)
        {
            if record.fingerprint != request.fingerprint {
                return Ok(DeliveryClaim::Refused(format!(
                    "operation id `{}` was already used for a different effect",
                    request.operation_id
                )));
            }
            if let Some(answer) = data.cached.get(&request.operation_id) {
                return Ok(DeliveryClaim::Cached(answer.clone()));
            }
            return Ok(DeliveryClaim::Unknown(format!(
                "operation `{}` was accepted previously, but its terminal response is unavailable",
                request.operation_id
            )));
        }

        while data.records.len() >= MAX_DELIVERY_RECORDS {
            let Some(index) = data
                .records
                .iter()
                .position(|record| record.state != DeliveryState::Accepted)
            else {
                return Ok(DeliveryClaim::Refused(format!(
                    "remote delivery ledger is full with {MAX_DELIVERY_RECORDS} in-flight operations"
                )));
            };
            let removed = data.records.remove(index);
            data.cached.remove(&removed.operation_id);
        }
        data.records.push(DeliveryRecord {
            operation_id: request.operation_id.clone(),
            fingerprint: request.fingerprint.clone(),
            state: DeliveryState::Accepted,
            accepted_at_ms: now_millis()?,
        });
        while encoded_records_len(&data.records)? > MAX_DELIVERY_LEDGER_BYTES {
            let Some(index) = data
                .records
                .iter()
                .position(|record| record.state != DeliveryState::Accepted)
            else {
                data.records.pop();
                return Ok(DeliveryClaim::Refused(format!(
                    "remote delivery ledger reached its {MAX_DELIVERY_LEDGER_BYTES}-byte bound"
                )));
            };
            let removed = data.records.remove(index);
            data.cached.remove(&removed.operation_id);
        }
        self.persist(data)?;
        Ok(DeliveryClaim::Execute)
    }

    async fn finish(&self, operation_id: &str, answer: WireAnswer) -> Result<()> {
        let mut slot = self.data.lock().await;
        self.load_if_needed(&mut slot).await?;
        let data = slot.as_mut().expect("delivery ledger initialized");
        let record = data
            .records
            .iter_mut()
            .find(|record| record.operation_id == operation_id)
            .ok_or_else(|| Error::Other(format!("delivery record `{operation_id}` disappeared")))?;
        record.state = DeliveryState::Completed;
        data.cached.insert(operation_id.to_string(), answer);
        self.persist(data)
    }

    async fn load_if_needed(&self, slot: &mut Option<DeliveryData>) -> Result<()> {
        if slot.is_some() {
            return Ok(());
        }
        let mut data = if self.system.path_exists(DELIVERY_LEDGER_PATH).await? {
            let text = self.system.read_file(DELIVERY_LEDGER_PATH).await?;
            DeliveryData {
                records: serde_json::from_str(&text).map_err(|error| {
                    Error::Other(format!("remote delivery ledger is invalid: {error}"))
                })?,
                cached: HashMap::new(),
            }
        } else {
            DeliveryData::default()
        };
        let mut changed = false;
        for record in &mut data.records {
            if record.state == DeliveryState::Accepted {
                record.state = DeliveryState::Unknown;
                changed = true;
            }
        }
        if changed {
            self.persist(&data)?;
        }
        *slot = Some(data);
        Ok(())
    }

    fn persist(&self, data: &DeliveryData) -> Result<()> {
        let encoded = serde_json::to_string(&data.records)
            .map_err(|error| Error::Other(format!("encode remote delivery ledger: {error}")))?;
        self.system
            .write_file_atomic(DELIVERY_LEDGER_PATH, &encoded)
    }

    async fn status(&self, operation_id: &str) -> Result<WireAnswer> {
        let mut slot = self.data.lock().await;
        self.load_if_needed(&mut slot).await?;
        let data = slot.as_ref().expect("delivery ledger initialized");
        let Some(record) = data
            .records
            .iter()
            .find(|record| record.operation_id == operation_id)
        else {
            return Ok(refused(format!("unknown operation id `{operation_id}`")));
        };
        if let Some(answer) = data.cached.get(operation_id) {
            return Ok(answer.clone());
        }
        Ok(WireAnswer {
            status: WireStatus::Unknown,
            value: None,
            detail: Some(format!(
                "operation `{operation_id}` is {:?}; no terminal response is available",
                record.state
            )),
        })
    }
}

fn encoded_records_len(records: &[DeliveryRecord]) -> Result<usize> {
    serde_json::to_vec(records)
        .map(|encoded| encoded.len())
        .map_err(|error| Error::Other(format!("encode remote delivery ledger: {error}")))
}

/// Build the authenticated remote-system router. Every route is protected.
fn remote_system_router(
    system: Arc<System>,
    auth: ServerAuth,
    bind: SocketAddr,
) -> anyhow::Result<Router> {
    guard_open_bind(&auth, bind)?;
    let identity = system.substrate_identity();
    // Declared once, at bind time: which kinds this substrate can measure is a property of the
    // machine and its build, not of a request, so a caller learns it from the handshake instead of
    // paying a round trip for it.
    let metric_kinds: Vec<String> = GuardedMetrics::served_metric_kinds(&*system)
        .into_iter()
        .map(|kind| kind.as_str().to_string())
        .collect();
    let state = SystemState {
        delivery: Arc::new(DeliveryLedger::new(system.clone())),
        network: Arc::new(NetworkResources::default()),
        system,
        handshake: SystemHandshake {
            protocol_version: PROTOCOL_VERSION,
            substrate_kind: identity.kind,
            workspace: identity.workspace,
            confinement: identity.confinement,
            operations: bounded_operations()
                .iter()
                .map(|operation| (*operation).to_string())
                .collect(),
            metric_kinds,
        },
    };
    let auth = Arc::new(auth);
    Ok(Router::new()
        .route("/system/v1/handshake", get(handshake))
        .route("/system/v1/execute", post(execute))
        .route(
            "/system/v1/operations/{operation_id}",
            get(operation_status),
        )
        .route("/system/v1/process", get(process_upgrade))
        .route("/system/v1/network", get(network_upgrade))
        .with_state(state)
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            REQUEST_TIMEOUT,
        ))
        .route_layer(middleware::from_fn_with_state(auth, require_auth)))
}

/// Serve one canonical workspace over TLS until shutdown.
pub async fn serve_tls(
    bind: SocketAddr,
    system: Arc<System>,
    token: String,
    cert_pem: impl AsRef<Path>,
    key_pem: impl AsRef<Path>,
) -> anyhow::Result<()> {
    if token.trim().is_empty() {
        anyhow::bail!("remote-system daemon requires a non-empty bearer token");
    }
    ensure_crypto_provider();
    let app = remote_system_router(system, ServerAuth::from_token(Some(token)), bind)?;
    let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert_pem, key_pem).await?;
    axum_server::bind_rustls(bind, tls)
        .serve(app.into_make_service())
        .await?;
    Ok(())
}

/// The complete product graph enables both Rustls providers (Slack brings `ring`; reqwest brings
/// AWS-LC), so Rustls cannot choose one automatically. Installing the workspace's intended provider
/// is idempotent: an embedder that selected a provider first keeps its selection.
///
/// Public because anything that stands up TLS in this workspace has to make the same choice — a
/// test's stand-in far side included, and a process that reaches one without ever calling `serve`
/// would otherwise panic inside Rustls rather than fail a check it can see.
pub fn ensure_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

async fn handshake(State(state): State<SystemState>) -> Json<SystemHandshake> {
    Json(state.handshake)
}

async fn operation_status(
    State(state): State<SystemState>,
    AxumPath(operation_id): AxumPath<String>,
) -> Response {
    match state.delivery.status(&operation_id).await {
        Ok(answer) => Json(answer).into_response(),
        Err(error) => Json(refused(error.to_string())).into_response(),
    }
}

async fn process_upgrade(
    State(state): State<SystemState>,
    upgrade: WebSocketUpgrade,
) -> impl IntoResponse {
    upgrade
        .max_message_size(MAX_REQUEST_BYTES)
        .max_frame_size(MAX_REQUEST_BYTES)
        .on_upgrade(move |socket| serve_process_socket(socket, state.system))
}

#[derive(Debug, Serialize, Deserialize)]
struct ProcessStart {
    argv: Vec<String>,
    #[serde(default)]
    env: Vec<(String, String)>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ProcessFrame {
    Started,
    Output {
        stdout: String,
        stderr: String,
    },
    Status {
        running: bool,
        exit_code: Option<i32>,
    },
    Kill,
    Error {
        detail: String,
    },
}

async fn serve_process_socket(mut socket: WebSocket, system: Arc<System>) {
    let Some(Ok(ServerWsMessage::Text(start))) = socket.next().await else {
        return;
    };
    let start: ProcessStart = match serde_json::from_str(start.as_str()) {
        Ok(start) => start,
        Err(error) => {
            let _ = send_process_frame(
                &mut socket,
                ProcessFrame::Error {
                    detail: format!("invalid process start frame: {error}"),
                },
            )
            .await;
            return;
        }
    };
    let mut child = match system.spawn_background(&start.argv, &start.env) {
        Ok(child) => child,
        Err(error) => {
            let _ = send_process_frame(
                &mut socket,
                ProcessFrame::Error {
                    detail: error.to_string(),
                },
            )
            .await;
            return;
        }
    };
    if send_process_frame(&mut socket, ProcessFrame::Started)
        .await
        .is_err()
    {
        return;
    }

    let mut ticker = tokio::time::interval(Duration::from_millis(20));
    loop {
        tokio::select! {
            incoming = socket.next() => match incoming {
                Some(Ok(ServerWsMessage::Text(text))) => {
                    if matches!(serde_json::from_str(text.as_str()), Ok(ProcessFrame::Kill)) {
                        child.kill();
                    }
                }
                Some(Ok(ServerWsMessage::Close(_))) | None | Some(Err(_)) => return,
                _ => {}
            },
            _ = ticker.tick() => {
                let (stdout, stderr) = child.read_output();
                if (!stdout.is_empty() || !stderr.is_empty())
                    && send_process_frame(&mut socket, ProcessFrame::Output { stdout, stderr }).await.is_err()
                {
                    return;
                }
                let status = child.status();
                if send_process_frame(
                    &mut socket,
                    ProcessFrame::Status {
                        running: status.running,
                        exit_code: status.exit_code,
                    },
                )
                .await
                .is_err()
                {
                    return;
                }
                if !status.running {
                    let (stdout, stderr) = child.read_output();
                    if !stdout.is_empty() || !stderr.is_empty() {
                        let _ = send_process_frame(
                            &mut socket,
                            ProcessFrame::Output { stdout, stderr },
                        )
                        .await;
                    }
                    return;
                }
            }
        }
    }
}

async fn send_process_frame(
    socket: &mut WebSocket,
    frame: ProcessFrame,
) -> std::result::Result<(), ()> {
    let text = serde_json::to_string(&frame).map_err(|_| ())?;
    socket
        .send(ServerWsMessage::Text(text.into()))
        .await
        .map_err(|_| ())
}

async fn network_upgrade(
    State(state): State<SystemState>,
    upgrade: WebSocketUpgrade,
) -> impl IntoResponse {
    upgrade
        .max_message_size(MAX_REQUEST_BYTES)
        .max_frame_size(MAX_REQUEST_BYTES)
        .on_upgrade(move |socket| serve_network_socket(socket, state))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum WireDialTarget {
    Tcp { host: String, port: u16 },
    Unix { path: String },
    Udp { host: String, port: u16 },
    Icmp { host: String },
}

impl From<&DialTarget> for WireDialTarget {
    fn from(target: &DialTarget) -> Self {
        match target {
            DialTarget::Tcp { host, port } => Self::Tcp {
                host: host.clone(),
                port: *port,
            },
            DialTarget::Unix { path } => Self::Unix { path: path.clone() },
            DialTarget::Udp { host, port } => Self::Udp {
                host: host.clone(),
                port: *port,
            },
            DialTarget::Icmp { host } => Self::Icmp { host: host.clone() },
        }
    }
}

impl From<WireDialTarget> for DialTarget {
    fn from(target: WireDialTarget) -> Self {
        match target {
            WireDialTarget::Tcp { host, port } => Self::Tcp { host, port },
            WireDialTarget::Unix { path } => Self::Unix { path },
            WireDialTarget::Udp { host, port } => Self::Udp { host, port },
            WireDialTarget::Icmp { host } => Self::Icmp { host },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "hosts", rename_all = "snake_case")]
enum WirePrivateAllow {
    None,
    Hosts(Vec<String>),
    Any,
}

impl From<&PrivateNetAllow> for WirePrivateAllow {
    fn from(allow: &PrivateNetAllow) -> Self {
        match allow {
            PrivateNetAllow::None => Self::None,
            PrivateNetAllow::Hosts(hosts) => Self::Hosts(hosts.clone()),
            PrivateNetAllow::Any => Self::Any,
        }
    }
}

impl From<WirePrivateAllow> for PrivateNetAllow {
    fn from(allow: WirePrivateAllow) -> Self {
        match allow {
            WirePrivateAllow::None => Self::None,
            WirePrivateAllow::Hosts(hosts) => Self::from_hosts(hosts),
            WirePrivateAllow::Any => Self::Any,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireExposure {
    LoopbackOnly,
    Authenticated,
}

impl From<BindExposure> for WireExposure {
    fn from(exposure: BindExposure) -> Self {
        match exposure {
            BindExposure::LoopbackOnly => Self::LoopbackOnly,
            BindExposure::Authenticated => Self::Authenticated,
        }
    }
}

impl From<WireExposure> for BindExposure {
    fn from(exposure: WireExposure) -> Self {
        match exposure {
            WireExposure::LoopbackOnly => Self::LoopbackOnly,
            WireExposure::Authenticated => Self::Authenticated,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct WireLimits {
    max_connections: usize,
    max_frame_bytes: usize,
    io_timeout_ms: u64,
}

impl From<InboundLimits> for WireLimits {
    fn from(limits: InboundLimits) -> Self {
        Self {
            max_connections: limits.max_connections,
            max_frame_bytes: limits.max_frame_bytes,
            io_timeout_ms: u64::try_from(limits.io_timeout.as_millis()).unwrap_or(u64::MAX),
        }
    }
}

impl From<WireLimits> for InboundLimits {
    fn from(limits: WireLimits) -> Self {
        Self {
            max_connections: limits.max_connections,
            max_frame_bytes: limits.max_frame_bytes,
            io_timeout: Duration::from_millis(limits.io_timeout_ms),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum NetworkFrame {
    Dial {
        target: WireDialTarget,
        allow: WirePrivateAllow,
    },
    BindTcp {
        addr: SocketAddr,
        exposure: WireExposure,
        limits: WireLimits,
    },
    BindUdp {
        addr: SocketAddr,
        exposure: WireExposure,
        limits: WireLimits,
        allow: WirePrivateAllow,
    },
    Attach {
        handle: String,
    },
    Started {
        local_addr: Option<SocketAddr>,
    },
    Read {
        max: usize,
    },
    Write {
        data: String,
    },
    Shutdown,
    Data {
        data: String,
        peer: Option<SocketAddr>,
    },
    ReadError {
        detail: String,
    },
    WriteError {
        detail: String,
    },
    Accept,
    Accepted {
        handle: String,
        peer: SocketAddr,
    },
    RecvFrom,
    SendTo {
        data: String,
        host: String,
        port: u16,
    },
    Ok,
    Error {
        detail: String,
    },
}

async fn serve_network_socket(mut socket: WebSocket, state: SystemState) {
    let Some(Ok(ServerWsMessage::Text(start))) = socket.next().await else {
        return;
    };
    let start = match serde_json::from_str::<NetworkFrame>(start.as_str()) {
        Ok(frame) => frame,
        Err(error) => {
            let _ = send_network_frame(
                &mut socket,
                NetworkFrame::Error {
                    detail: format!("invalid network start frame: {error}"),
                },
            )
            .await;
            return;
        }
    };
    match start {
        NetworkFrame::Dial { target, allow } => {
            match state
                .system
                .dial_scoped(&target.into(), &allow.into())
                .await
            {
                Ok(stream) => serve_stream_socket(socket, stream).await,
                Err(error) => {
                    let _ = send_network_frame(
                        &mut socket,
                        NetworkFrame::Error {
                            detail: error.to_string(),
                        },
                    )
                    .await;
                }
            }
        }
        NetworkFrame::Attach { handle } => {
            let stream = state.network.streams.lock().await.remove(&handle);
            match stream.filter(|pending| pending.expires_at > Instant::now()) {
                Some(pending) => serve_stream_socket(socket, pending.stream).await,
                None => {
                    let _ = send_network_frame(
                        &mut socket,
                        NetworkFrame::Error {
                            detail: "unknown or already-attached network handle".into(),
                        },
                    )
                    .await;
                }
            }
        }
        NetworkFrame::BindTcp {
            addr,
            exposure,
            limits,
        } => match state
            .system
            .bind_tcp(addr, exposure.into(), limits.into())
            .await
        {
            Ok(listener) => serve_listener_socket(socket, listener, state.network).await,
            Err(error) => {
                let _ = send_network_frame(
                    &mut socket,
                    NetworkFrame::Error {
                        detail: error.to_string(),
                    },
                )
                .await;
            }
        },
        NetworkFrame::BindUdp {
            addr,
            exposure,
            limits,
            allow,
        } => match state
            .system
            .bind_udp(addr, exposure.into(), limits.into(), allow.into())
            .await
        {
            Ok(endpoint) => serve_datagram_socket(socket, endpoint).await,
            Err(error) => {
                let _ = send_network_frame(
                    &mut socket,
                    NetworkFrame::Error {
                        detail: error.to_string(),
                    },
                )
                .await;
            }
        },
        _ => {
            let _ = send_network_frame(
                &mut socket,
                NetworkFrame::Error {
                    detail: "expected a network start frame".into(),
                },
            )
            .await;
        }
    }
}

enum StreamWriteCommand {
    Write(Vec<u8>),
    Shutdown,
}

async fn serve_stream_socket(mut socket: WebSocket, stream: NetworkStream) {
    if send_network_frame(&mut socket, NetworkFrame::Started { local_addr: None })
        .await
        .is_err()
    {
        return;
    }
    let (mut stream_read, mut stream_write) = stream.into_split();
    let (read_tx, mut read_rx) = tokio::sync::mpsc::channel::<usize>(1);
    let (write_tx, mut write_rx) = tokio::sync::mpsc::channel::<StreamWriteCommand>(1);
    let (response_tx, mut response_rx) = tokio::sync::mpsc::channel::<NetworkFrame>(2);
    let read_responses = response_tx.clone();
    let read_worker = tokio::spawn(async move {
        while let Some(max) = read_rx.recv().await {
            let response = match stream_read.read(max).await {
                Ok(data) => NetworkFrame::Data {
                    data: encode_bytes(&data),
                    peer: None,
                },
                Err(error) => NetworkFrame::ReadError {
                    detail: error.to_string(),
                },
            };
            if read_responses.send(response).await.is_err() {
                break;
            }
        }
    });
    let write_worker = tokio::spawn(async move {
        while let Some(command) = write_rx.recv().await {
            let result = match command {
                StreamWriteCommand::Write(data) => stream_write.write_all(&data).await,
                StreamWriteCommand::Shutdown => stream_write.shutdown().await,
            };
            let response = match result {
                Ok(()) => NetworkFrame::Ok,
                Err(error) => NetworkFrame::WriteError {
                    detail: error.to_string(),
                },
            };
            if response_tx.send(response).await.is_err() {
                break;
            }
        }
    });
    let (mut socket_tx, mut socket_rx) = socket.split();
    loop {
        tokio::select! {
            command = socket_rx.next() => {
                let Some(Ok(ServerWsMessage::Text(text))) = command else { break };
                let routed = match serde_json::from_str::<NetworkFrame>(text.as_str()) {
                    Ok(NetworkFrame::Read { max }) => read_tx.send(max).await.map_err(|_| ()),
                    Ok(NetworkFrame::Write { data }) => match decode_bytes(Some(&Value::String(data))) {
                        Ok(data) => write_tx.send(StreamWriteCommand::Write(data)).await.map_err(|_| ()),
                        Err(detail) => {
                            if socket_tx.send(ServerWsMessage::Text(
                                serde_json::to_string(&NetworkFrame::WriteError { detail }).unwrap().into()
                            )).await.is_err() { break; }
                            continue;
                        }
                    },
                    Ok(NetworkFrame::Shutdown) => write_tx.send(StreamWriteCommand::Shutdown).await.map_err(|_| ()),
                    _ => {
                        if socket_tx.send(ServerWsMessage::Text(
                            serde_json::to_string(&NetworkFrame::Error { detail: "invalid stream command".into() }).unwrap().into()
                        )).await.is_err() { break; }
                        continue;
                    }
                };
                if routed.is_err() { break; }
            }
            response = response_rx.recv() => {
                let Some(response) = response else { break };
                let Ok(text) = serde_json::to_string(&response) else { break };
                if socket_tx.send(ServerWsMessage::Text(text.into())).await.is_err() { break; }
            }
        }
    }
    read_worker.abort();
    write_worker.abort();
}

async fn serve_listener_socket(
    mut socket: WebSocket,
    mut listener: NetworkListener,
    resources: Arc<NetworkResources>,
) {
    let local_addr = match listener.local_addr() {
        Ok(addr) => addr,
        Err(error) => {
            let _ = send_network_frame(
                &mut socket,
                NetworkFrame::Error {
                    detail: error.to_string(),
                },
            )
            .await;
            return;
        }
    };
    if send_network_frame(
        &mut socket,
        NetworkFrame::Started {
            local_addr: Some(local_addr),
        },
    )
    .await
    .is_err()
    {
        return;
    }
    while let Some(Ok(ServerWsMessage::Text(text))) = socket.next().await {
        let response = match serde_json::from_str::<NetworkFrame>(text.as_str()) {
            Ok(NetworkFrame::Accept) => match listener.accept().await {
                Ok((stream, peer)) => {
                    let id = resources.next_id.fetch_add(1, Ordering::Relaxed);
                    let handle = format!("stream-{id}");
                    let mut streams = resources.streams.lock().await;
                    let now = Instant::now();
                    streams.retain(|_, pending| pending.expires_at > now);
                    if streams.len() >= MAX_PENDING_STREAMS {
                        NetworkFrame::Error {
                            detail: format!(
                                "remote network handle registry reached its {MAX_PENDING_STREAMS}-stream bound"
                            ),
                        }
                    } else {
                        streams.insert(
                            handle.clone(),
                            PendingStream {
                                stream,
                                expires_at: now + PENDING_STREAM_TTL,
                            },
                        );
                        NetworkFrame::Accepted { handle, peer }
                    }
                }
                Err(error) => NetworkFrame::Error {
                    detail: error.to_string(),
                },
            },
            Ok(NetworkFrame::Shutdown) => {
                listener.close();
                NetworkFrame::Ok
            }
            _ => NetworkFrame::Error {
                detail: "invalid listener command".into(),
            },
        };
        if send_network_frame(&mut socket, response).await.is_err() {
            return;
        }
    }
}

async fn serve_datagram_socket(mut socket: WebSocket, mut endpoint: DatagramEndpoint) {
    let local_addr = match endpoint.local_addr() {
        Ok(addr) => addr,
        Err(error) => {
            let _ = send_network_frame(
                &mut socket,
                NetworkFrame::Error {
                    detail: error.to_string(),
                },
            )
            .await;
            return;
        }
    };
    if send_network_frame(
        &mut socket,
        NetworkFrame::Started {
            local_addr: Some(local_addr),
        },
    )
    .await
    .is_err()
    {
        return;
    }
    while let Some(Ok(ServerWsMessage::Text(text))) = socket.next().await {
        let response = match serde_json::from_str::<NetworkFrame>(text.as_str()) {
            Ok(NetworkFrame::RecvFrom) => match endpoint.recv_from().await {
                Ok((data, peer)) => NetworkFrame::Data {
                    data: encode_bytes(&data),
                    peer: Some(peer),
                },
                Err(error) => NetworkFrame::Error {
                    detail: error.to_string(),
                },
            },
            Ok(NetworkFrame::SendTo { data, host, port }) => {
                match decode_bytes(Some(&Value::String(data))) {
                    Ok(data) => match endpoint.send_to(&data, &host, port).await {
                        Ok(()) => NetworkFrame::Ok,
                        Err(error) => NetworkFrame::Error {
                            detail: error.to_string(),
                        },
                    },
                    Err(detail) => NetworkFrame::Error { detail },
                }
            }
            Ok(NetworkFrame::Shutdown) => {
                endpoint.close();
                NetworkFrame::Ok
            }
            _ => NetworkFrame::Error {
                detail: "invalid datagram command".into(),
            },
        };
        if send_network_frame(&mut socket, response).await.is_err() {
            return;
        }
    }
}

async fn send_network_frame(
    socket: &mut WebSocket,
    frame: NetworkFrame,
) -> std::result::Result<(), ()> {
    let text = serde_json::to_string(&frame).map_err(|_| ())?;
    socket
        .send(ServerWsMessage::Text(text.into()))
        .await
        .map_err(|_| ())
}

async fn execute(State(state): State<SystemState>, Json(request): Json<WireRequest>) -> Response {
    if request.protocol_version != PROTOCOL_VERSION {
        return (
            StatusCode::BAD_REQUEST,
            Json(WireAnswer {
                status: WireStatus::Refused,
                value: None,
                detail: Some(format!(
                    "unsupported remote-system protocol version {}",
                    request.protocol_version
                )),
            }),
        )
            .into_response();
    }
    if fingerprint(&request.operation, &request.arguments) != request.fingerprint {
        return (
            StatusCode::BAD_REQUEST,
            Json(WireAnswer {
                status: WireStatus::Refused,
                value: None,
                detail: Some("operation fingerprint does not match the request".into()),
            }),
        )
            .into_response();
    }
    match state.delivery.claim(&request).await {
        Ok(DeliveryClaim::Execute) => {}
        Ok(DeliveryClaim::Cached(answer)) => return Json(answer).into_response(),
        Ok(DeliveryClaim::Refused(detail)) => return Json(refused(detail)).into_response(),
        Ok(DeliveryClaim::Unknown(detail)) => {
            return Json(WireAnswer {
                status: WireStatus::Unknown,
                value: None,
                detail: Some(detail),
            })
            .into_response();
        }
        Err(error) => return Json(refused(error.to_string())).into_response(),
    }
    let operation_id = request.operation_id;
    let answer = dispatch(&state.system, &request.operation, request.arguments).await;
    if let Err(error) = state.delivery.finish(&operation_id, answer.clone()).await {
        return Json(WireAnswer {
            status: WireStatus::Unknown,
            value: None,
            detail: Some(format!(
                "operation `{operation_id}` ran, but its terminal receipt could not be recorded: {error}"
            )),
        })
        .into_response();
    }
    Json(answer).into_response()
}

async fn dispatch(system: &System, operation: &str, arguments: Value) -> WireAnswer {
    macro_rules! arg {
        ($name:literal, $method:ident) => {
            match arguments.get($name).and_then(Value::$method) {
                Some(value) => value,
                None => return refused(format!("{operation}: missing or invalid `{}`", $name)),
            }
        };
    }

    let answer = match operation {
        "process.run" => {
            let argv = match strings(arguments.get("argv")) {
                Ok(value) => value,
                Err(error) => return refused(error),
            };
            let env = match pairs(arguments.get("env")) {
                Ok(value) => value,
                Err(error) => return refused(error),
            };
            let timeout = Duration::from_millis(arg!("timeout_ms", as_u64));
            served_result(
                system
                    .run_with_env(&argv, &env, timeout)
                    .await
                    .map(process_value),
            )
        }
        "process.run_stdin" => {
            let argv = match strings(arguments.get("argv")) {
                Ok(value) => value,
                Err(error) => return refused(error),
            };
            let stdin = match decode_bytes(arguments.get("stdin")) {
                Ok(value) => value,
                Err(error) => return refused(error),
            };
            let timeout = Duration::from_millis(arg!("timeout_ms", as_u64));
            served_result(
                system
                    .run_with_stdin(&argv, &stdin, timeout)
                    .await
                    .map(process_value),
            )
        }
        "env.read" => served(json!(system.env(arg!("key", as_str)))),
        // C-654. `kind` is optional and that is the whole shape: with it, one measurement; without
        // it, the substrate's snapshot in one round trip, because eight separately-timed requests
        // would compose readings from different moments into a picture of none of them. Either way
        // the answer is bounded by the closed vocabulary, so there is no size argument to police.
        "host.metrics" => {
            let requested = match arguments.get("kind") {
                None | Some(Value::Null) => None,
                Some(Value::String(token)) => match MetricKind::from_token(token) {
                    Some(kind) => Some(kind),
                    // A token this build does not know is refused rather than approximated: the
                    // vocabulary is closed, so the nearest kind is still the wrong instrument.
                    None => {
                        return refused(format!(
                            "host.metrics: `{token}` is not a metric kind (known: {})",
                            metric_kind_tokens()
                        ))
                    }
                },
                Some(_) => return refused("host.metrics: `kind` must be a string"),
            };
            let read = match requested {
                Some(kind) => GuardedMetrics::read_metric(system, kind)
                    .await
                    .map(|answer| vec![answer]),
                None => GuardedMetrics::read_metrics(system).await,
            };
            served_result(read.and_then(|answers| metric_answers_value(&answers)))
        }
        "host.identity" => served_result(
            system
                .host_path_identity(arg!("path", as_str))
                .map(Value::String),
        ),
        "host.read" => {
            let max = match usize::try_from(arg!("max_bytes", as_u64)) {
                Ok(value) => value,
                Err(_) => return refused("host.read: `max_bytes` is too large"),
            };
            served_result(
                system
                    .read_file_scoped(arg!("path", as_str), arg!("scope", as_str), max)
                    .await
                    .map(scoped_read_value),
            )
        }
        "workspace.read_bytes" => served_result(
            system
                .read_file_bytes(arg!("path", as_str))
                .await
                .map(|bytes| json!(encode_bytes(&bytes))),
        ),
        "workspace.write_bytes" => {
            let bytes = match decode_bytes(arguments.get("contents")) {
                Ok(value) => value,
                Err(error) => return refused(error),
            };
            served_result(
                system
                    .write_file_bytes(arg!("path", as_str), &bytes)
                    .await
                    .map(|()| Value::Null),
            )
        }
        "workspace.append" => served_result(
            system
                .append_file(arg!("path", as_str), arg!("contents", as_str))
                .await
                .map(|()| Value::Null),
        ),
        "workspace.read_capped" => {
            let max = match usize::try_from(arg!("max", as_u64)) {
                Ok(value) => value,
                Err(_) => return refused("workspace.read_capped: `max` is too large"),
            };
            served_result(
                system
                    .read_file_bytes_capped(arg!("path", as_str), max)
                    .await
                    .map(|(bytes, truncated)| {
                        json!({"bytes": encode_bytes(&bytes), "truncated": truncated})
                    }),
            )
        }
        "workspace.file_size" => served_result(
            system
                .file_size(arg!("path", as_str))
                .await
                .map(|value| json!(value)),
        ),
        "workspace.path_exists" => served_result(
            system
                .path_exists(arg!("path", as_str))
                .await
                .map(|value| json!(value)),
        ),
        "workspace.is_dir" => served_result(
            system
                .is_dir(arg!("path", as_str))
                .await
                .map(|value| json!(value)),
        ),
        "workspace.file_mtime" => served_result(
            system
                .file_mtime(arg!("path", as_str))
                .await
                .and_then(system_time_millis)
                .map(|value| json!(value)),
        ),
        "workspace.list_dir" => served_result(
            system
                .list_dir(arg!("path", as_str))
                .await
                .map(|value| json!(value)),
        ),
        "workspace.walk_files" => {
            let max = match usize::try_from(arg!("max", as_u64)) {
                Ok(value) => value,
                Err(_) => return refused("workspace.walk_files: `max` is too large"),
            };
            served_result(
                system
                    .walk_files(arg!("base", as_str), max)
                    .await
                    .map(|value| json!(value)),
            )
        }
        _ => WireAnswer {
            status: WireStatus::Unserved,
            value: None,
            detail: Some(format!("operation `{operation}`")),
        },
    };
    answer
}

fn bounded_operations() -> &'static [&'static str] {
    &[
        "process.run",
        "process.run_stdin",
        "process.spawn",
        "network.dial",
        "network.bind_tcp",
        "network.bind_udp",
        "env.read",
        "host.identity",
        "host.metrics",
        "host.read",
        "workspace.read_bytes",
        "workspace.write_bytes",
        "workspace.append",
        "workspace.read_capped",
        "workspace.file_size",
        "workspace.path_exists",
        "workspace.is_dir",
        "workspace.file_mtime",
        "workspace.list_dir",
        "workspace.walk_files",
    ]
}

fn served(value: Value) -> WireAnswer {
    WireAnswer {
        status: WireStatus::Served,
        value: Some(value),
        detail: None,
    }
}

fn refused(detail: impl Into<String>) -> WireAnswer {
    WireAnswer {
        status: WireStatus::Refused,
        value: None,
        detail: Some(detail.into()),
    }
}

fn served_result(result: Result<Value>) -> WireAnswer {
    match result {
        Ok(value) => served(value),
        Err(error) => match flux_system::remote::failure_mode(&error) {
            Some(flux_system::remote::FailureMode::Unserved) => WireAnswer {
                status: WireStatus::Unserved,
                value: None,
                detail: Some(error.to_string()),
            },
            _ => refused(error.to_string()),
        },
    }
}

// ---------------------------------------------------------------------------
// host.metrics — the wire form of the closed metric vocabulary (C-654)
// ---------------------------------------------------------------------------
//
// Hand-written rather than derived, for two reasons that are really one. The typed vocabulary lives
// in `flux-system` and stays free of a serialization format — the port is a Rust trait, not a
// protocol — so the encoding belongs here beside the other wire shapes (`process_value`,
// `WireDialTarget`). And a derived `Deserialize` would be a decoder that *accepts whatever the
// bytes say*, which is exactly wrong for this family: the caps on labels, mounts and sensors are a
// construction-site convention over public fields, so the only place they can be re-imposed on a
// reading measured by another machine is at the point of decode.
//
// `remotely_reported` is deliberately absent from the wire. It describes *the hop*, and
// `RemoteSystem` sets it; a peer that could put it in a frame could claim its numbers were read
// locally.

/// Every token the closed vocabulary answers to, for a refusal that tells an operator what to ask.
fn metric_kind_tokens() -> String {
    MetricKind::ALL
        .iter()
        .map(|kind| kind.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn metric_reading_value(reading: &MetricReading) -> Value {
    match reading {
        MetricReading::CpuUsage(usage) => json!({
            "logical_cores": usage.logical_cores,
            "busy_ratio": usage.busy_ratio,
            "window_ms": u64::try_from(usage.window.as_millis()).unwrap_or(u64::MAX),
        }),
        MetricReading::LoadAverage(load) => json!({
            "one_minute": load.one_minute,
            "five_minute": load.five_minute,
            "fifteen_minute": load.fifteen_minute,
        }),
        MetricReading::Memory(pool) | MetricReading::Swap(pool) => json!({
            "total_bytes": pool.total_bytes,
            "available_bytes": pool.available_bytes,
            "used_bytes": pool.used_bytes,
        }),
        MetricReading::Disk(mounts) => Value::Array(
            mounts
                .iter()
                .map(|mount| {
                    json!({
                        "mount_point": mount.mount_point,
                        "filesystem": mount.filesystem,
                        "total_bytes": mount.total_bytes,
                        "available_bytes": mount.available_bytes,
                        "used_bytes": mount.used_bytes,
                    })
                })
                .collect(),
        ),
        MetricReading::Uptime(uptime) => json!({
            "uptime_ms": u64::try_from(uptime.as_millis()).unwrap_or(u64::MAX),
        }),
        MetricReading::Temperature(sensors) => Value::Array(
            sensors
                .iter()
                .map(|sensor| json!({"label": sensor.label, "celsius": sensor.celsius}))
                .collect(),
        ),
        MetricReading::FanSpeed(sensors) => Value::Array(
            sensors
                .iter()
                .map(|sensor| json!({"label": sensor.label, "rpm": sensor.rpm}))
                .collect(),
        ),
    }
}

/// One bounded snapshot as the wire carries it. Fails only where a sample time cannot be expressed
/// — never by dropping an answer, since a missing answer and an unavailable one read differently.
fn metric_answers_value(answers: &[MetricAnswer]) -> Result<Value> {
    let mut encoded = Vec::with_capacity(answers.len());
    for answer in answers {
        encoded.push(match answer {
            MetricAnswer::Served(snapshot) => json!({
                "kind": snapshot.kind().as_str(),
                "status": "served",
                "sampled_at_ms": system_time_millis(snapshot.sampled_at)?,
                "reading": metric_reading_value(&snapshot.reading),
            }),
            MetricAnswer::Unavailable { kind, reason } => json!({
                "kind": kind.as_str(),
                "status": "unavailable",
                "reason": reason.as_str(),
            }),
        });
    }
    Ok(json!({"answers": encoded}))
}

fn number(value: &Value, key: &str) -> std::result::Result<f64, String> {
    value
        .get(key)
        .and_then(Value::as_f64)
        .filter(|number| number.is_finite())
        .ok_or_else(|| format!("metric reading is missing a finite `{key}`"))
}

fn count(value: &Value, key: &str) -> std::result::Result<u64, String> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("metric reading is missing `{key}`"))
}

fn label(value: &Value, key: &str) -> std::result::Result<String, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(bounded_label)
        .ok_or_else(|| format!("metric reading is missing `{key}`"))
}

/// The entries of an encoded list reading, truncated to `max` **before** anything is built.
///
/// The cap is applied to the iterator rather than to the finished `Vec` on purpose: truncating
/// afterwards would already have allocated whatever the far side sent.
fn capped_entries(value: &Value, max: usize) -> std::result::Result<Vec<&Value>, String> {
    Ok(value
        .as_array()
        .ok_or_else(|| "metric reading is not a list".to_string())?
        .iter()
        .take(max)
        .collect())
}

fn decode_metric_reading(
    kind: MetricKind,
    value: &Value,
) -> std::result::Result<MetricReading, String> {
    let pool = |value: &Value| -> std::result::Result<MemoryUsage, String> {
        Ok(MemoryUsage {
            total_bytes: count(value, "total_bytes")?,
            available_bytes: count(value, "available_bytes")?,
            used_bytes: count(value, "used_bytes")?,
        })
    };
    Ok(match kind {
        MetricKind::CpuUsage => MetricReading::CpuUsage(CpuUsage {
            logical_cores: u32::try_from(count(value, "logical_cores")?)
                .map_err(|_| "cpu reports an implausible core count".to_string())?,
            // A ratio outside `0.0..=1.0` is not a fraction of a window; clamping keeps a
            // projection's arithmetic sane without discarding an otherwise usable reading.
            busy_ratio: number(value, "busy_ratio")?.clamp(0.0, 1.0),
            window: Duration::from_millis(count(value, "window_ms")?),
        }),
        MetricKind::LoadAverage => MetricReading::LoadAverage(LoadAverage {
            one_minute: number(value, "one_minute")?,
            five_minute: number(value, "five_minute")?,
            fifteen_minute: number(value, "fifteen_minute")?,
        }),
        MetricKind::Memory => MetricReading::Memory(pool(value)?),
        MetricKind::Swap => MetricReading::Swap(pool(value)?),
        MetricKind::Disk => MetricReading::Disk(
            capped_entries(value, MAX_MOUNTS)?
                .into_iter()
                .map(|mount| {
                    Ok(MountUsage {
                        mount_point: label(mount, "mount_point")?,
                        filesystem: label(mount, "filesystem")?,
                        total_bytes: count(mount, "total_bytes")?,
                        available_bytes: count(mount, "available_bytes")?,
                        used_bytes: count(mount, "used_bytes")?,
                    })
                })
                .collect::<std::result::Result<Vec<_>, String>>()?,
        ),
        MetricKind::Uptime => {
            MetricReading::Uptime(Duration::from_millis(count(value, "uptime_ms")?))
        }
        MetricKind::Temperature => MetricReading::Temperature(
            capped_entries(value, MAX_SENSORS)?
                .into_iter()
                .map(|sensor| {
                    Ok(TemperatureSensor {
                        label: label(sensor, "label")?,
                        celsius: number(sensor, "celsius")?,
                    })
                })
                .collect::<std::result::Result<Vec<_>, String>>()?,
        ),
        MetricKind::FanSpeed => MetricReading::FanSpeed(
            capped_entries(value, MAX_SENSORS)?
                .into_iter()
                .map(|sensor| {
                    Ok(FanSensor {
                        label: label(sensor, "label")?,
                        rpm: u32::try_from(count(sensor, "rpm")?)
                            .map_err(|_| "a fan reports an implausible rpm".to_string())?,
                    })
                })
                .collect::<std::result::Result<Vec<_>, String>>()?,
        ),
    })
}

/// Decode a `host.metrics` answer list, re-imposing every bound the vocabulary declares.
///
/// The list itself is capped by the closed vocabulary — a peer cannot make a caller iterate over
/// more answers than there are kinds — and each answer's `remotely_reported` is left `false` here
/// because setting it is `RemoteSystem`'s job, not the transport's.
fn decode_metric_answers(value: Value) -> std::result::Result<Vec<MetricAnswer>, String> {
    let encoded = value
        .get("answers")
        .and_then(Value::as_array)
        .ok_or_else(|| "host.metrics response is missing `answers`".to_string())?;
    let mut answers = Vec::with_capacity(encoded.len().min(MetricKind::ALL.len()));
    for answer in encoded.iter().take(MetricKind::ALL.len()) {
        let token = answer
            .get("kind")
            .and_then(Value::as_str)
            .ok_or_else(|| "a metric answer is missing `kind`".to_string())?;
        let kind = MetricKind::from_token(token)
            .ok_or_else(|| format!("`{token}` is not a metric kind this build knows"))?;
        match answer.get("status").and_then(Value::as_str) {
            Some("served") => {
                let sampled_at =
                    UNIX_EPOCH + Duration::from_millis(count(answer, "sampled_at_ms")?);
                let reading = answer
                    .get("reading")
                    .ok_or_else(|| format!("`{token}` is served with no reading"))?;
                answers.push(MetricAnswer::Served(MetricSnapshot {
                    sampled_at,
                    reading: decode_metric_reading(kind, reading)?,
                    remotely_reported: false,
                }));
            }
            Some("unavailable") => {
                let reason = answer
                    .get("reason")
                    .and_then(Value::as_str)
                    .and_then(MetricUnavailable::from_token)
                    .ok_or_else(|| format!("`{token}` is unavailable for no stated reason"))?;
                answers.push(MetricAnswer::unavailable_for(kind, reason));
            }
            // Never a fabricated zero and never a silent drop: a status this build cannot read is
            // an answer it must not pretend to have understood.
            other => {
                return Err(format!(
                    "`{token}` carries an unreadable metric status {other:?}"
                ))
            }
        }
    }
    Ok(answers)
}

fn process_value(output: ProcessOutput) -> Value {
    json!({"stdout": output.stdout, "stderr": output.stderr, "exit_code": output.exit_code})
}

fn scoped_read_value(read: ScopedFileRead) -> Value {
    json!({"bytes": encode_bytes(&read.bytes), "size": read.size, "truncated": read.truncated})
}

fn encode_bytes(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn decode_bytes(value: Option<&Value>) -> std::result::Result<Vec<u8>, String> {
    let encoded = value
        .and_then(Value::as_str)
        .ok_or_else(|| "missing or invalid base64 byte payload".to_string())?;
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|error| format!("invalid base64 byte payload: {error}"))
}

fn strings(value: Option<&Value>) -> std::result::Result<Vec<String>, String> {
    value
        .and_then(Value::as_array)
        .ok_or_else(|| "missing or invalid string array".to_string())?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| "invalid string array member".to_string())
        })
        .collect()
}

fn pairs(value: Option<&Value>) -> std::result::Result<Vec<(String, String)>, String> {
    value
        .and_then(Value::as_array)
        .ok_or_else(|| "missing or invalid environment pair array".to_string())?
        .iter()
        .map(|value| {
            let pair = value
                .as_array()
                .filter(|pair| pair.len() == 2)
                .ok_or_else(|| "invalid environment pair".to_string())?;
            Ok((
                pair[0]
                    .as_str()
                    .ok_or_else(|| "invalid environment key".to_string())?
                    .to_string(),
                pair[1]
                    .as_str()
                    .ok_or_else(|| "invalid environment value".to_string())?
                    .to_string(),
            ))
        })
        .collect()
}

fn system_time_millis(value: SystemTime) -> Result<u64> {
    value
        .duration_since(UNIX_EPOCH)
        .map_err(|error| Error::Other(format!("file mtime predates the Unix epoch: {error}")))
        .and_then(|duration| {
            u64::try_from(duration.as_millis())
                .map_err(|_| Error::Other("file mtime does not fit the wire format".into()))
        })
}

fn now_millis() -> Result<u64> {
    system_time_millis(SystemTime::now())
}

fn fingerprint(operation: &str, arguments: &Value) -> String {
    let mut digest = Sha256::new();
    digest.update(operation.as_bytes());
    digest.update([0]);
    digest.update(serde_json::to_vec(arguments).unwrap_or_default());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest.finalize())
}

/// HTTPS implementation of [`Delegate`] for bounded request/response operations.
pub struct HttpDelegate {
    http: reqwest::Client,
    execute_url: reqwest::Url,
    process_ws_url: reqwest::Url,
    network_ws_url: reqwest::Url,
    ws: WsTransport,
    token: String,
    client_id: String,
    next_id: AtomicU64,
    /// The metric kinds the peer declared at handshake, resolved against the closed vocabulary
    /// (C-654). Empty means the peer serves no metrics — see [`HttpDelegate::metrics_gap`].
    metric_kinds: Vec<MetricKind>,
}

impl HttpDelegate {
    /// Connect through the shared URL guard and ordinary certificate validation.
    pub async fn connect(
        endpoint: &str,
        token: String,
        private_net: &flux_system::net::PrivateNetAllow,
    ) -> Result<(Arc<Self>, SystemHandshake)> {
        Self::connect_with_builder(
            endpoint,
            token,
            private_net,
            reqwest::Client::builder(),
            websocket_connector(None)?,
        )
        .await
    }

    /// Connect while trusting one additional PEM certificate. Intended for private deployments
    /// whose daemon certificate chains to an operator-managed CA.
    pub async fn connect_with_ca_pem(
        endpoint: &str,
        token: String,
        private_net: &flux_system::net::PrivateNetAllow,
        ca_pem: &[u8],
    ) -> Result<(Arc<Self>, SystemHandshake)> {
        let certificate = reqwest::Certificate::from_pem(ca_pem)
            .map_err(|error| Error::Config(format!("remote-system CA certificate: {error}")))?;
        Self::connect_with_builder(
            endpoint,
            token,
            private_net,
            reqwest::Client::builder().add_root_certificate(certificate),
            websocket_connector(Some(ca_pem))?,
        )
        .await
    }

    async fn connect_with_builder(
        endpoint: &str,
        token: String,
        private_net: &flux_system::net::PrivateNetAllow,
        builder: reqwest::ClientBuilder,
        ws_connector: tokio_tungstenite::Connector,
    ) -> Result<(Arc<Self>, SystemHandshake)> {
        if token.trim().is_empty() {
            return Err(Error::Config(
                "remote-system client requires a non-empty bearer token".into(),
            ));
        }
        let (mut base, pinned) = flux_system::net::guard_url_scoped_pinned(endpoint, private_net)?;
        if base.scheme() != "https" {
            return Err(Error::Config(
                "remote-system endpoint must use https".into(),
            ));
        }
        base.set_path("/");
        base.set_query(None);
        base.set_fragment(None);
        let host = base
            .host_str()
            .ok_or_else(|| Error::Config("remote-system endpoint has no host".into()))?;
        let http = builder
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .resolve_to_addrs(host, &pinned)
            .build()
            .map_err(|error| Error::Other(format!("build remote-system client: {error}")))?;
        let execute_url = base
            .join("system/v1/execute")
            .map_err(|error| Error::Config(format!("remote-system endpoint: {error}")))?;
        let mut process_ws_url = base
            .join("system/v1/process")
            .map_err(|error| Error::Config(format!("remote-system endpoint: {error}")))?;
        process_ws_url
            .set_scheme("wss")
            .map_err(|_| Error::Config("remote-system WebSocket URL is invalid".into()))?;
        let mut network_ws_url = base
            .join("system/v1/network")
            .map_err(|error| Error::Config(format!("remote-system endpoint: {error}")))?;
        network_ws_url
            .set_scheme("wss")
            .map_err(|_| Error::Config("remote-system WebSocket URL is invalid".into()))?;
        let handshake_url = base
            .join("system/v1/handshake")
            .map_err(|error| Error::Config(format!("remote-system endpoint: {error}")))?;
        let handshake = http
            .get(handshake_url)
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|error| Error::Other(format!("remote-system handshake: {error}")))?
            .error_for_status()
            .map_err(|error| Error::Other(format!("remote-system handshake: {error}")))?
            .json::<SystemHandshake>()
            .await
            .map_err(|error| Error::Other(format!("remote-system handshake frame: {error}")))?;
        // A mixed pair is refused outright rather than degraded to the operations both sides
        // happen to share. The two versions disagree about what a frame *means*, not only about
        // which frames exist, so a partial pairing would put the disagreement inside individual
        // operations — where it surfaces mid-effect — instead of at the one place an operator can
        // act on it.
        if handshake.protocol_version != PROTOCOL_VERSION {
            return Err(Error::Config(format!(
                "remote-system protocol mismatch: local {PROTOCOL_VERSION}, remote {}",
                handshake.protocol_version
            )));
        }
        let metric_kinds = handshake.declared_metric_kinds();
        Ok((
            Arc::new(Self {
                http,
                execute_url,
                process_ws_url,
                network_ws_url,
                ws: WsTransport {
                    pinned,
                    token: token.clone(),
                    connector: ws_connector,
                },
                token,
                metric_kinds,
                client_id: format!(
                    "{}-{}-{}",
                    std::process::id(),
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_err(|error| Error::Other(format!("system clock: {error}")))?
                        .as_nanos(),
                    CLIENT_INSTANCE_SEQ.fetch_add(1, Ordering::Relaxed)
                ),
                next_id: AtomicU64::new(1),
            }),
            handshake,
        ))
    }

    async fn request(&self, operation: &str, arguments: Value) -> Delivered<Value> {
        let sequence = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = WireRequest {
            protocol_version: PROTOCOL_VERSION,
            operation_id: format!("{}-{sequence}", self.client_id),
            fingerprint: fingerprint(operation, &arguments),
            operation: operation.to_string(),
            arguments,
        };
        let response = self
            .http
            .post(self.execute_url.clone())
            .bearer_auth(&self.token)
            .json(&request)
            .send()
            .await
            .map_err(|error| Unreachable::new(error.to_string()))?;
        if !response.status().is_success() {
            return Ok(Answer::Refused(format!(
                "remote-system HTTP status {}",
                response.status()
            )));
        }
        let answer = response
            .json::<WireAnswer>()
            .await
            .map_err(|error| Unreachable::new(format!("invalid response frame: {error}")))?;
        match answer.status {
            WireStatus::Served => Ok(Answer::Served(answer.value.unwrap_or(Value::Null))),
            WireStatus::Refused => Ok(Answer::Refused(
                answer.detail.unwrap_or_else(|| "remote refused".into()),
            )),
            WireStatus::Unserved => Ok(Answer::Unserved(
                answer.detail.unwrap_or_else(|| operation.to_string()),
            )),
            WireStatus::Unknown => Ok(Answer::Unknown(
                answer
                    .detail
                    .unwrap_or_else(|| "remote outcome is unknown".into()),
            )),
        }
    }

    fn request_blocking(&self, operation: &'static str, arguments: Value) -> Delivered<Value> {
        let this = self.clone_for_thread();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| Unreachable::new(error.to_string()))?;
            runtime.block_on(this.request(operation, arguments))
        })
        .join()
        .map_err(|_| Unreachable::new("remote-system request thread panicked"))?
    }

    fn clone_for_thread(&self) -> Self {
        Self {
            http: self.http.clone(),
            execute_url: self.execute_url.clone(),
            process_ws_url: self.process_ws_url.clone(),
            network_ws_url: self.network_ws_url.clone(),
            ws: self.ws.clone(),
            token: self.token.clone(),
            client_id: self.client_id.clone(),
            next_id: AtomicU64::new(self.next_id.fetch_add(1, Ordering::Relaxed)),
            metric_kinds: self.metric_kinds.clone(),
        }
    }

    /// The typed answer for a peer that declared no metric vocabulary, or `None` when it did.
    ///
    /// This is where "an older server answers a typed unsupported" actually happens. A peer that
    /// does not serve the family declares no kinds — an older build has no `metric_kinds` field at
    /// all, and a same-version peer on a platform with no reader declares an empty one — and both
    /// are answered here, without a request. Sending one anyway would turn a known capability gap
    /// into a round trip whose failure a caller has to interpret, and the mode matters: `Unserved`
    /// means *implement it or stop asking*, so a retry loop must never be told to try again.
    fn metrics_gap<T>(&self) -> Option<Delivered<T>> {
        self.metric_kinds.is_empty().then(|| {
            Ok(Answer::Unserved(
                "measure its own substrate — the remote peer declared no metric kinds in its \
                 handshake"
                    .to_string(),
            ))
        })
    }

    async fn open_process_socket(&self, start: &ProcessStart) -> Delivered<ProcessSocket> {
        let mut socket = self.ws.open(&self.process_ws_url).await?;
        let frame =
            serde_json::to_string(start).map_err(|error| Unreachable::new(error.to_string()))?;
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(frame.into()))
            .await
            .map_err(|error| Unreachable::new(error.to_string()))?;
        match socket.next().await {
            Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                match serde_json::from_str::<ProcessFrame>(text.as_str()) {
                    Ok(ProcessFrame::Started) => Ok(Answer::Served(socket)),
                    Ok(ProcessFrame::Error { detail }) => Ok(Answer::Refused(detail)),
                    Ok(_) => Err(Unreachable::new(
                        "remote process sent data before its start receipt",
                    )),
                    Err(error) => Err(Unreachable::new(format!(
                        "invalid remote process start frame: {error}"
                    ))),
                }
            }
            Some(Err(error)) => Err(Unreachable::new(error.to_string())),
            _ => Err(Unreachable::new(
                "remote process stream closed before its start receipt",
            )),
        }
    }

    async fn open_network_socket(
        &self,
        start: NetworkFrame,
    ) -> Delivered<(ProcessSocket, Option<SocketAddr>)> {
        open_network_socket(&self.ws, &self.network_ws_url, start).await
    }
}

async fn open_network_socket(
    transport: &WsTransport,
    url: &reqwest::Url,
    start: NetworkFrame,
) -> Delivered<(ProcessSocket, Option<SocketAddr>)> {
    let mut socket = transport.open(url).await?;
    send_client_network_frame(&mut socket, start).await?;
    match receive_client_network_frame(&mut socket).await? {
        NetworkFrame::Started { local_addr } => Ok(Answer::Served((socket, local_addr))),
        NetworkFrame::Error { detail } => Ok(Answer::Refused(detail)),
        _ => Err(Unreachable::new(
            "remote network stream sent data before its start receipt",
        )),
    }
}

async fn send_client_network_frame(
    socket: &mut ProcessSocket,
    frame: NetworkFrame,
) -> std::result::Result<(), Unreachable> {
    let text =
        serde_json::to_string(&frame).map_err(|error| Unreachable::new(error.to_string()))?;
    socket
        .send(tokio_tungstenite::tungstenite::Message::Text(text.into()))
        .await
        .map_err(|error| Unreachable::new(error.to_string()))
}

async fn receive_client_network_frame(
    socket: &mut ProcessSocket,
) -> std::result::Result<NetworkFrame, Unreachable> {
    match socket.next().await {
        Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
            serde_json::from_str(text.as_str())
                .map_err(|error| Unreachable::new(format!("invalid network frame: {error}")))
        }
        Some(Err(error)) => Err(Unreachable::new(error.to_string())),
        _ => Err(Unreachable::new("remote network stream closed")),
    }
}

async fn network_rpc(
    socket: &Arc<tokio::sync::Mutex<ProcessSocket>>,
    command: NetworkFrame,
) -> Result<NetworkFrame> {
    let mut socket = socket.lock().await;
    send_client_network_frame(&mut socket, command)
        .await
        .map_err(|error| Error::Other(error.to_string()))?;
    match receive_client_network_frame(&mut socket)
        .await
        .map_err(|error| Error::Other(error.to_string()))?
    {
        NetworkFrame::Error { detail } => {
            Err(GuardedIoError::new(GuardedIoFailure::Refused, detail).into())
        }
        response => Ok(response),
    }
}

async fn network_rpc_direct(
    socket: &mut ProcessSocket,
    command: NetworkFrame,
) -> Result<NetworkFrame> {
    send_client_network_frame(socket, command)
        .await
        .map_err(|error| Error::Other(error.to_string()))?;
    match receive_client_network_frame(socket)
        .await
        .map_err(|error| Error::Other(error.to_string()))?
    {
        NetworkFrame::Error { detail }
        | NetworkFrame::ReadError { detail }
        | NetworkFrame::WriteError { detail } => {
            Err(GuardedIoError::new(GuardedIoFailure::Refused, detail).into())
        }
        response => Ok(response),
    }
}

struct RemoteDuplexStream {
    socket: ProcessSocket,
}

impl GuardedDuplex for RemoteDuplexStream {
    fn read<'a>(&'a mut self, max: usize) -> flux_system::port::Guarded<'a, Vec<u8>> {
        Box::pin(async move {
            match network_rpc_direct(&mut self.socket, NetworkFrame::Read { max }).await? {
                NetworkFrame::Data { data, peer: None } => {
                    decode_bytes(Some(&Value::String(data))).map_err(Error::Other)
                }
                _ => Err(Error::Other("invalid remote stream read response".into())),
            }
        })
    }

    fn write_all<'a>(&'a mut self, data: &'a [u8]) -> flux_system::port::Guarded<'a, ()> {
        Box::pin(async move {
            match network_rpc_direct(
                &mut self.socket,
                NetworkFrame::Write {
                    data: encode_bytes(data),
                },
            )
            .await?
            {
                NetworkFrame::Ok => Ok(()),
                _ => Err(Error::Other("invalid remote stream write response".into())),
            }
        })
    }

    fn shutdown<'a>(&'a mut self) -> flux_system::port::Guarded<'a, ()> {
        Box::pin(async move {
            match network_rpc_direct(&mut self.socket, NetworkFrame::Shutdown).await? {
                NetworkFrame::Ok => Ok(()),
                _ => Err(Error::Other(
                    "invalid remote stream shutdown response".into(),
                )),
            }
        })
    }

    fn split(self: Box<Self>) -> (Box<dyn DuplexReadHalf>, Box<dyn DuplexWriteHalf>) {
        split_remote_stream(self.socket)
    }
}

struct RemoteReadRequest {
    max: usize,
    response: tokio::sync::oneshot::Sender<Result<Vec<u8>>>,
}

enum RemoteWriteKind {
    Write(Vec<u8>),
    Shutdown,
}

struct RemoteWriteRequest {
    kind: RemoteWriteKind,
    response: tokio::sync::oneshot::Sender<Result<()>>,
}

struct RemoteReadHalf {
    requests: tokio::sync::mpsc::Sender<RemoteReadRequest>,
    cancel: tokio_util::sync::CancellationToken,
}

impl Drop for RemoteReadHalf {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl DuplexReadHalf for RemoteReadHalf {
    fn read<'a>(&'a mut self, max: usize) -> flux_system::port::Guarded<'a, Vec<u8>> {
        Box::pin(async move {
            let (response, answer) = tokio::sync::oneshot::channel();
            self.requests
                .send(RemoteReadRequest { max, response })
                .await
                .map_err(|_| Error::Other("remote network stream closed".into()))?;
            answer
                .await
                .map_err(|_| Error::Other("remote network stream closed".into()))?
        })
    }
}

struct RemoteWriteHalf {
    requests: tokio::sync::mpsc::Sender<RemoteWriteRequest>,
    cancel: tokio_util::sync::CancellationToken,
}

impl Drop for RemoteWriteHalf {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl RemoteWriteHalf {
    async fn request(&self, kind: RemoteWriteKind) -> Result<()> {
        let (response, answer) = tokio::sync::oneshot::channel();
        self.requests
            .send(RemoteWriteRequest { kind, response })
            .await
            .map_err(|_| Error::Other("remote network stream closed".into()))?;
        answer
            .await
            .map_err(|_| Error::Other("remote network stream closed".into()))?
    }
}

impl DuplexWriteHalf for RemoteWriteHalf {
    fn write_all<'a>(&'a mut self, data: &'a [u8]) -> flux_system::port::Guarded<'a, ()> {
        Box::pin(async move { self.request(RemoteWriteKind::Write(data.to_vec())).await })
    }

    fn shutdown<'a>(&'a mut self) -> flux_system::port::Guarded<'a, ()> {
        Box::pin(async move { self.request(RemoteWriteKind::Shutdown).await })
    }
}

fn split_remote_stream(
    socket: ProcessSocket,
) -> (Box<dyn DuplexReadHalf>, Box<dyn DuplexWriteHalf>) {
    let (read_tx, read_rx) = tokio::sync::mpsc::channel(1);
    let (write_tx, write_rx) = tokio::sync::mpsc::channel(1);
    let cancel = tokio_util::sync::CancellationToken::new();
    tokio::spawn(drive_remote_stream(
        socket,
        read_rx,
        write_rx,
        cancel.clone(),
    ));
    (
        Box::new(RemoteReadHalf {
            requests: read_tx,
            cancel: cancel.clone(),
        }),
        Box::new(RemoteWriteHalf {
            requests: write_tx,
            cancel,
        }),
    )
}

async fn drive_remote_stream(
    socket: ProcessSocket,
    mut read_requests: tokio::sync::mpsc::Receiver<RemoteReadRequest>,
    mut write_requests: tokio::sync::mpsc::Receiver<RemoteWriteRequest>,
    cancel: tokio_util::sync::CancellationToken,
) {
    use tokio_tungstenite::tungstenite::Message;

    let (mut socket_tx, mut socket_rx) = socket.split();
    let mut pending_read: Option<tokio::sync::oneshot::Sender<Result<Vec<u8>>>> = None;
    let mut pending_write: Option<tokio::sync::oneshot::Sender<Result<()>>> = None;
    let mut read_open = true;
    let mut write_open = true;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            request = read_requests.recv(), if read_open && pending_read.is_none() => {
                let Some(request) = request else {
                    read_open = false;
                    if !write_open && pending_write.is_none() { break; }
                    continue;
                };
                let frame = NetworkFrame::Read { max: request.max };
                let sent = serde_json::to_string(&frame)
                    .map(Message::text)
                    .map_err(|error| error.to_string());
                match sent {
                    Ok(message) => {
                        if socket_tx.send(message).await.is_ok() {
                            pending_read = Some(request.response);
                        } else {
                            let _ = request.response.send(Err(Error::Other("remote network stream closed".into())));
                            break;
                        }
                    }
                    Err(detail) => {
                        let _ = request.response.send(Err(Error::Other(detail)));
                    }
                }
            }
            request = write_requests.recv(), if write_open && pending_write.is_none() => {
                let Some(request) = request else {
                    write_open = false;
                    if !read_open && pending_read.is_none() { break; }
                    continue;
                };
                let frame = match &request.kind {
                    RemoteWriteKind::Write(data) => NetworkFrame::Write { data: encode_bytes(data) },
                    RemoteWriteKind::Shutdown => NetworkFrame::Shutdown,
                };
                let sent = serde_json::to_string(&frame)
                    .map(Message::text)
                    .map_err(|error| error.to_string());
                match sent {
                    Ok(message) => {
                        if socket_tx.send(message).await.is_ok() {
                            pending_write = Some(request.response);
                        } else {
                            let _ = request.response.send(Err(Error::Other("remote network stream closed".into())));
                            break;
                        }
                    }
                    Err(detail) => {
                        let _ = request.response.send(Err(Error::Other(detail)));
                    }
                }
            }
            response = socket_rx.next() => {
                let Some(Ok(Message::Text(text))) = response else { break };
                match serde_json::from_str::<NetworkFrame>(text.as_str()) {
                    Ok(NetworkFrame::Data { data, peer: None }) => {
                        if let Some(response) = pending_read.take() {
                            let result = decode_bytes(Some(&Value::String(data))).map_err(Error::Other);
                            let _ = response.send(result);
                        }
                    }
                    Ok(NetworkFrame::ReadError { detail }) => {
                        if let Some(response) = pending_read.take() {
                            let _ = response.send(Err(
                                GuardedIoError::new(GuardedIoFailure::Refused, detail).into()
                            ));
                        }
                    }
                    Ok(NetworkFrame::Ok) => {
                        if let Some(response) = pending_write.take() {
                            let _ = response.send(Ok(()));
                        }
                    }
                    Ok(NetworkFrame::WriteError { detail }) => {
                        if let Some(response) = pending_write.take() {
                            let _ = response.send(Err(
                                GuardedIoError::new(GuardedIoFailure::Refused, detail).into()
                            ));
                        }
                    }
                    Ok(NetworkFrame::Error { detail }) => {
                        if let Some(response) = pending_read.take() {
                            let _ = response.send(Err(
                                GuardedIoError::new(GuardedIoFailure::Refused, detail.clone()).into()
                            ));
                        }
                        if let Some(response) = pending_write.take() {
                            let _ = response.send(Err(
                                GuardedIoError::new(GuardedIoFailure::Refused, detail).into()
                            ));
                        }
                    }
                    Err(error) => {
                        let detail = format!("invalid network frame: {error}");
                        if let Some(response) = pending_read.take() {
                            let _ = response.send(Err(Error::Other(detail.clone())));
                        }
                        if let Some(response) = pending_write.take() {
                            let _ = response.send(Err(Error::Other(detail)));
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

struct RemoteStreamListener {
    socket: Arc<tokio::sync::Mutex<ProcessSocket>>,
    local_addr: SocketAddr,
    transport: WsTransport,
    network_url: reqwest::Url,
}

impl StreamListener for RemoteStreamListener {
    fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.local_addr)
    }

    fn accept<'a>(&'a mut self) -> flux_system::port::Guarded<'a, (NetworkStream, SocketAddr)> {
        Box::pin(async move {
            let response = network_rpc(&self.socket, NetworkFrame::Accept).await?;
            let NetworkFrame::Accepted { handle, peer } = response else {
                return Err(Error::Other(
                    "invalid remote listener accept response".into(),
                ));
            };
            let (socket, _) = delivered_to_result(
                open_network_socket(
                    &self.transport,
                    &self.network_url,
                    NetworkFrame::Attach { handle },
                )
                .await,
            )?;
            Ok((
                NetworkStream::from_handle(RemoteDuplexStream { socket }),
                peer,
            ))
        })
    }

    fn close(&mut self) {
        spawn_network_close(self.socket.clone());
    }
}

struct RemoteDatagramEndpoint {
    socket: Arc<tokio::sync::Mutex<ProcessSocket>>,
    local_addr: SocketAddr,
}

impl DatagramHandle for RemoteDatagramEndpoint {
    fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.local_addr)
    }

    fn recv_from<'a>(&'a mut self) -> flux_system::port::Guarded<'a, (Vec<u8>, SocketAddr)> {
        Box::pin(async move {
            match network_rpc(&self.socket, NetworkFrame::RecvFrom).await? {
                NetworkFrame::Data {
                    data,
                    peer: Some(peer),
                } => Ok((
                    decode_bytes(Some(&Value::String(data))).map_err(Error::Other)?,
                    peer,
                )),
                _ => Err(Error::Other(
                    "invalid remote datagram receive response".into(),
                )),
            }
        })
    }

    fn send_to<'a>(
        &'a mut self,
        data: &'a [u8],
        host: &'a str,
        port: u16,
    ) -> flux_system::port::Guarded<'a, ()> {
        Box::pin(async move {
            match network_rpc(
                &self.socket,
                NetworkFrame::SendTo {
                    data: encode_bytes(data),
                    host: host.to_string(),
                    port,
                },
            )
            .await?
            {
                NetworkFrame::Ok => Ok(()),
                _ => Err(Error::Other("invalid remote datagram send response".into())),
            }
        })
    }

    fn close(&mut self) {
        spawn_network_close(self.socket.clone());
    }
}

fn spawn_network_close(socket: Arc<tokio::sync::Mutex<ProcessSocket>>) {
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn(async move {
            let _ = network_rpc(&socket, NetworkFrame::Shutdown).await;
        });
    }
}

type ProcessSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

#[derive(Clone)]
struct WsTransport {
    pinned: Vec<SocketAddr>,
    token: String,
    connector: tokio_tungstenite::Connector,
}

impl WsTransport {
    async fn open(&self, url: &reqwest::Url) -> std::result::Result<ProcessSocket, Unreachable> {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;

        let mut last_error = None;
        for address in self.pinned.iter().copied() {
            match tokio::net::TcpStream::connect(address).await {
                Ok(stream) => {
                    let mut request = url
                        .as_str()
                        .into_client_request()
                        .map_err(|error| Unreachable::new(error.to_string()))?;
                    request.headers_mut().insert(
                        tokio_tungstenite::tungstenite::http::header::AUTHORIZATION,
                        format!("Bearer {}", self.token)
                            .parse()
                            .map_err(|error| Unreachable::new(format!("bearer header: {error}")))?,
                    );
                    match tokio_tungstenite::client_async_tls_with_config(
                        request,
                        stream,
                        Some(
                            tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
                                .write_buffer_size(64 * 1024)
                                .max_write_buffer_size(MAX_REQUEST_BYTES)
                                .max_message_size(Some(MAX_REQUEST_BYTES))
                                .max_frame_size(Some(MAX_REQUEST_BYTES)),
                        ),
                        Some(self.connector.clone()),
                    )
                    .await
                    {
                        Ok((socket, _)) => return Ok(socket),
                        Err(error) => last_error = Some(error.to_string()),
                    }
                }
                Err(error) => last_error = Some(error.to_string()),
            }
        }
        Err(Unreachable::new(last_error.unwrap_or_else(|| {
            "no vetted remote-system address was connectable".into()
        })))
    }
}

fn websocket_connector(ca_pem: Option<&[u8]>) -> Result<tokio_tungstenite::Connector> {
    use rustls::pki_types::{pem::PemObject, CertificateDer};

    ensure_crypto_provider();
    let mut roots =
        rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(pem) = ca_pem {
        for certificate in CertificateDer::pem_slice_iter(pem) {
            roots
                .add(certificate.map_err(|error| {
                    Error::Config(format!("remote-system CA certificate: {error}"))
                })?)
                .map_err(|error| Error::Config(format!("remote-system CA certificate: {error}")))?;
        }
    }
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(tokio_tungstenite::Connector::Rustls(Arc::new(config)))
}

#[derive(Default)]
struct RemoteProcessState {
    stdout: String,
    stderr: String,
    running: bool,
    exit_code: Option<i32>,
}

struct RemoteManagedProcess {
    state: Arc<std::sync::Mutex<RemoteProcessState>>,
    kill: tokio::sync::mpsc::UnboundedSender<()>,
}

impl ManagedProcess for RemoteManagedProcess {
    fn read_output(&mut self) -> (String, String) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        (
            std::mem::take(&mut state.stdout),
            std::mem::take(&mut state.stderr),
        )
    }

    fn status(&mut self) -> ChildStatus {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        ChildStatus {
            running: state.running,
            exit_code: state.exit_code,
        }
    }

    fn kill(&mut self) {
        let _ = self.kill.send(());
    }
}

async fn drive_remote_process(
    mut socket: ProcessSocket,
    state: Arc<std::sync::Mutex<RemoteProcessState>>,
    mut kill: tokio::sync::mpsc::UnboundedReceiver<()>,
) {
    loop {
        tokio::select! {
            command = kill.recv() => {
                if command.is_none() {
                    let _ = socket.close(None).await;
                    break;
                }
                let frame = serde_json::to_string(&ProcessFrame::Kill).unwrap_or_else(|_| "{\"type\":\"kill\"}".into());
                if socket.send(tokio_tungstenite::tungstenite::Message::Text(frame.into())).await.is_err() {
                    break;
                }
            }
            frame = socket.next() => match frame {
                Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                    match serde_json::from_str::<ProcessFrame>(text.as_str()) {
                        Ok(ProcessFrame::Output { stdout, stderr }) => {
                            let mut current = state.lock().unwrap_or_else(|error| error.into_inner());
                            current.stdout.push_str(&stdout);
                            current.stderr.push_str(&stderr);
                        }
                        Ok(ProcessFrame::Status { running, exit_code }) => {
                            let mut current = state.lock().unwrap_or_else(|error| error.into_inner());
                            current.running = running;
                            current.exit_code = exit_code;
                        }
                        Ok(ProcessFrame::Error { detail }) => {
                            let mut current = state.lock().unwrap_or_else(|error| error.into_inner());
                            current.stderr.push_str(&detail);
                            current.running = false;
                            break;
                        }
                        _ => {}
                    }
                }
                Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            }
        }
    }
    state
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .running = false;
}

/// Connect and construct the selected remote execution system.
pub async fn connect_remote_system(
    endpoint: &str,
    token: String,
    private_net: &flux_system::net::PrivateNetAllow,
) -> Result<RemoteSystem> {
    let (delegate, handshake) = HttpDelegate::connect(endpoint, token, private_net).await?;
    Ok(RemoteSystem::identified(delegate, handshake.identity()))
}

/// Perform the remote protocol's identity handshake and return it without installing an
/// execution system — the side-effect-free `host probe` seam (C-649). The handshake is a GET of
/// the identity route; nothing executes on the substrate.
pub async fn probe_remote_system(
    endpoint: &str,
    token: String,
    private_net: &flux_system::net::PrivateNetAllow,
) -> Result<SystemHandshake> {
    let (_delegate, handshake) = HttpDelegate::connect(endpoint, token, private_net).await?;
    Ok(handshake)
}

/// Connect while trusting an operator-supplied private CA certificate.
pub async fn connect_remote_system_with_ca_pem(
    endpoint: &str,
    token: String,
    private_net: &flux_system::net::PrivateNetAllow,
    ca_pem: &[u8],
) -> Result<RemoteSystem> {
    let (delegate, handshake) =
        HttpDelegate::connect_with_ca_pem(endpoint, token, private_net, ca_pem).await?;
    Ok(RemoteSystem::identified(delegate, handshake.identity()))
}

fn settle_value<T>(
    delivered: Delivered<Value>,
    decode: impl FnOnce(Value) -> std::result::Result<T, String>,
) -> Delivered<T> {
    match delivered {
        Ok(Answer::Served(value)) => decode(value).map(Answer::Served).map_err(Unreachable::new),
        Ok(Answer::Refused(detail)) => Ok(Answer::Refused(detail)),
        Ok(Answer::Unserved(detail)) => Ok(Answer::Unserved(detail)),
        Ok(Answer::Unknown(detail)) => Ok(Answer::Unknown(detail)),
        Err(error) => Err(error),
    }
}

fn delivered_to_result<T>(delivered: Delivered<T>) -> Result<T> {
    match delivered {
        Ok(Answer::Served(value)) => Ok(value),
        Ok(Answer::Refused(detail)) => {
            Err(GuardedIoError::new(GuardedIoFailure::Refused, detail).into())
        }
        Ok(Answer::Unserved(detail)) => {
            Err(GuardedIoError::new(GuardedIoFailure::Unserved, detail).into())
        }
        Ok(Answer::Unknown(detail)) => {
            Err(GuardedIoError::new(GuardedIoFailure::Unknown, detail).into())
        }
        Err(error) => {
            Err(GuardedIoError::new(GuardedIoFailure::Unreachable, error.to_string()).into())
        }
    }
}

fn decode_json<T: serde::de::DeserializeOwned>(value: Value) -> std::result::Result<T, String> {
    serde_json::from_value(value).map_err(|error| error.to_string())
}

fn decode_process(value: Value) -> std::result::Result<ProcessOutput, String> {
    let stdout = value
        .get("stdout")
        .and_then(Value::as_str)
        .ok_or_else(|| "process response is missing `stdout`".to_string())?;
    let stderr = value
        .get("stderr")
        .and_then(Value::as_str)
        .ok_or_else(|| "process response is missing `stderr`".to_string())?;
    let exit_code = value
        .get("exit_code")
        .and_then(Value::as_i64)
        .and_then(|code| i32::try_from(code).ok())
        .ok_or_else(|| "process response has an invalid `exit_code`".to_string())?;
    Ok(ProcessOutput {
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
        exit_code,
    })
}

impl Delegate for HttpDelegate {
    fn run_with_env<'a>(
        &'a self,
        argv: &'a [String],
        env: &'a [(String, String)],
        timeout: Duration,
    ) -> Answered<'a, ProcessOutput> {
        Box::pin(async move {
            settle_value(
                self.request(
                    "process.run",
                    json!({"argv": argv, "env": env, "timeout_ms": timeout.as_millis()}),
                )
                .await,
                decode_process,
            )
        })
    }

    fn run_with_stdin<'a>(
        &'a self,
        argv: &'a [String],
        stdin: &'a [u8],
        timeout: Duration,
    ) -> Answered<'a, ProcessOutput> {
        Box::pin(async move {
            settle_value(
                self.request(
                    "process.run_stdin",
                    json!({
                        "argv": argv,
                        "stdin": encode_bytes(stdin),
                        "timeout_ms": timeout.as_millis()
                    }),
                )
                .await,
                decode_process,
            )
        })
    }

    fn spawn_background<'a>(
        &'a self,
        argv: &'a [String],
        env: &'a [(String, String)],
    ) -> Answered<'a, ManagedChild> {
        Box::pin(async move {
            let socket = match self
                .open_process_socket(&ProcessStart {
                    argv: argv.to_vec(),
                    env: env.to_vec(),
                })
                .await
            {
                Ok(Answer::Served(socket)) => socket,
                Ok(Answer::Refused(detail)) => return Ok(Answer::Refused(detail)),
                Ok(Answer::Unserved(detail)) => return Ok(Answer::Unserved(detail)),
                Ok(Answer::Unknown(detail)) => return Ok(Answer::Unknown(detail)),
                Err(error) => return Err(error),
            };
            let state = Arc::new(std::sync::Mutex::new(RemoteProcessState {
                running: true,
                ..RemoteProcessState::default()
            }));
            let (kill_tx, kill_rx) = tokio::sync::mpsc::unbounded_channel();
            tokio::spawn(drive_remote_process(socket, state.clone(), kill_rx));
            Ok(Answer::Served(ManagedChild::from_handle(
                RemoteManagedProcess {
                    state,
                    kill: kill_tx,
                },
            )))
        })
    }

    fn dial_scoped<'a>(
        &'a self,
        target: &'a DialTarget,
        allow: &'a PrivateNetAllow,
    ) -> Answered<'a, NetworkStream> {
        Box::pin(async move {
            let opened = self
                .open_network_socket(NetworkFrame::Dial {
                    target: target.into(),
                    allow: allow.into(),
                })
                .await?;
            let (socket, _) = match opened {
                Answer::Served(value) => value,
                Answer::Refused(detail) => return Ok(Answer::Refused(detail)),
                Answer::Unserved(detail) => return Ok(Answer::Unserved(detail)),
                Answer::Unknown(detail) => return Ok(Answer::Unknown(detail)),
            };
            Ok(Answer::Served(NetworkStream::from_handle(
                RemoteDuplexStream { socket },
            )))
        })
    }

    fn bind_tcp<'a>(
        &'a self,
        addr: SocketAddr,
        exposure: BindExposure,
        limits: InboundLimits,
    ) -> Answered<'a, NetworkListener> {
        Box::pin(async move {
            let opened = self
                .open_network_socket(NetworkFrame::BindTcp {
                    addr,
                    exposure: exposure.into(),
                    limits: limits.into(),
                })
                .await?;
            let (socket, local_addr) = match opened {
                Answer::Served(value) => value,
                Answer::Refused(detail) => return Ok(Answer::Refused(detail)),
                Answer::Unserved(detail) => return Ok(Answer::Unserved(detail)),
                Answer::Unknown(detail) => return Ok(Answer::Unknown(detail)),
            };
            let local_addr = local_addr
                .ok_or_else(|| Unreachable::new("remote listener did not report its address"))?;
            Ok(Answer::Served(NetworkListener::from_handle(
                RemoteStreamListener {
                    socket: Arc::new(tokio::sync::Mutex::new(socket)),
                    local_addr,
                    transport: self.ws.clone(),
                    network_url: self.network_ws_url.clone(),
                },
            )))
        })
    }

    fn bind_udp<'a>(
        &'a self,
        addr: SocketAddr,
        exposure: BindExposure,
        limits: InboundLimits,
        allow: PrivateNetAllow,
    ) -> Answered<'a, DatagramEndpoint> {
        Box::pin(async move {
            let opened = self
                .open_network_socket(NetworkFrame::BindUdp {
                    addr,
                    exposure: exposure.into(),
                    limits: limits.into(),
                    allow: (&allow).into(),
                })
                .await?;
            let (socket, local_addr) = match opened {
                Answer::Served(value) => value,
                Answer::Refused(detail) => return Ok(Answer::Refused(detail)),
                Answer::Unserved(detail) => return Ok(Answer::Unserved(detail)),
                Answer::Unknown(detail) => return Ok(Answer::Unknown(detail)),
            };
            let local_addr = local_addr
                .ok_or_else(|| Unreachable::new("remote datagram did not report its address"))?;
            Ok(Answer::Served(DatagramEndpoint::from_handle(
                RemoteDatagramEndpoint {
                    socket: Arc::new(tokio::sync::Mutex::new(socket)),
                    local_addr,
                },
            )))
        })
    }

    fn env(&self, key: &str) -> Delivered<Option<String>> {
        settle_value(
            self.request_blocking("env.read", json!({"key": key})),
            decode_json,
        )
    }

    fn host_path_identity(&self, path: &str) -> Delivered<String> {
        settle_value(
            self.request_blocking("host.identity", json!({"path": path})),
            decode_json,
        )
    }

    fn served_metric_kinds(&self) -> Vec<MetricKind> {
        self.metric_kinds.clone()
    }

    fn read_metric(&self, kind: MetricKind) -> Answered<'_, MetricAnswer> {
        Box::pin(async move {
            if let Some(gap) = self.metrics_gap() {
                return gap;
            }
            settle_value(
                self.request("host.metrics", json!({"kind": kind.as_str()}))
                    .await,
                |value| {
                    let answers = decode_metric_answers(value)?;
                    // Exactly one answer, and about the kind that was asked for. A peer that
                    // answered a *different* kind would have a caller render one instrument's
                    // measurement under another's name, which is worse than no reading at all.
                    match <[MetricAnswer; 1]>::try_from(answers) {
                        Ok([answer]) if answer.kind() == kind => Ok(answer),
                        Ok([answer]) => Err(format!(
                            "asked the remote substrate for `{kind}` and it answered about `{}`",
                            answer.kind()
                        )),
                        Err(answers) => Err(format!(
                            "asked the remote substrate for `{kind}` and it answered {} times",
                            answers.len()
                        )),
                    }
                },
            )
        })
    }

    fn read_metrics(&self) -> Answered<'_, Vec<MetricAnswer>> {
        Box::pin(async move {
            if let Some(gap) = self.metrics_gap() {
                return gap;
            }
            // No `kind`: one round trip for the whole snapshot, so every reading in it describes
            // the same moment.
            settle_value(
                self.request("host.metrics", json!({})).await,
                decode_metric_answers,
            )
        })
    }

    fn read_file_scoped<'a>(
        &'a self,
        path: &'a str,
        scope: &'a str,
        max_bytes: usize,
    ) -> Answered<'a, ScopedFileRead> {
        Box::pin(async move {
            settle_value(
                self.request(
                    "host.read",
                    json!({"path": path, "scope": scope, "max_bytes": max_bytes}),
                )
                .await,
                |value| {
                    #[derive(Deserialize)]
                    struct Read {
                        bytes: String,
                        size: u64,
                        truncated: bool,
                    }
                    let read: Read = decode_json(value)?;
                    Ok(ScopedFileRead {
                        bytes: decode_bytes(Some(&Value::String(read.bytes)))?,
                        size: read.size,
                        truncated: read.truncated,
                    })
                },
            )
        })
    }

    fn read_file_bytes<'a>(&'a self, path: &'a str) -> Answered<'a, Vec<u8>> {
        Box::pin(async move {
            settle_value(
                self.request("workspace.read_bytes", json!({"path": path}))
                    .await,
                |value| decode_bytes(Some(&value)),
            )
        })
    }

    fn write_file_bytes<'a>(&'a self, path: &'a str, contents: &'a [u8]) -> Answered<'a, ()> {
        Box::pin(async move {
            settle_value(
                self.request(
                    "workspace.write_bytes",
                    json!({"path": path, "contents": encode_bytes(contents)}),
                )
                .await,
                |_| Ok(()),
            )
        })
    }

    fn append_file<'a>(&'a self, path: &'a str, contents: &'a str) -> Answered<'a, ()> {
        Box::pin(async move {
            settle_value(
                self.request(
                    "workspace.append",
                    json!({"path": path, "contents": contents}),
                )
                .await,
                |_| Ok(()),
            )
        })
    }

    fn read_file_bytes_capped<'a>(
        &'a self,
        path: &'a str,
        max: usize,
    ) -> Answered<'a, (Vec<u8>, bool)> {
        Box::pin(async move {
            settle_value(
                self.request("workspace.read_capped", json!({"path": path, "max": max}))
                    .await,
                |value| {
                    let bytes = decode_bytes(value.get("bytes"))?;
                    let truncated = value
                        .get("truncated")
                        .and_then(Value::as_bool)
                        .ok_or_else(|| "missing `truncated`".to_string())?;
                    Ok((bytes, truncated))
                },
            )
        })
    }

    fn file_size<'a>(&'a self, path: &'a str) -> Answered<'a, u64> {
        Box::pin(async move {
            settle_value(
                self.request("workspace.file_size", json!({"path": path}))
                    .await,
                decode_json,
            )
        })
    }

    fn path_exists<'a>(&'a self, path: &'a str) -> Answered<'a, bool> {
        Box::pin(async move {
            settle_value(
                self.request("workspace.path_exists", json!({"path": path}))
                    .await,
                decode_json,
            )
        })
    }

    fn is_dir<'a>(&'a self, path: &'a str) -> Answered<'a, bool> {
        Box::pin(async move {
            settle_value(
                self.request("workspace.is_dir", json!({"path": path}))
                    .await,
                decode_json,
            )
        })
    }

    fn file_mtime<'a>(&'a self, path: &'a str) -> Answered<'a, SystemTime> {
        Box::pin(async move {
            settle_value(
                self.request("workspace.file_mtime", json!({"path": path}))
                    .await,
                |value| decode_json::<u64>(value).map(|ms| UNIX_EPOCH + Duration::from_millis(ms)),
            )
        })
    }

    fn list_dir<'a>(&'a self, path: &'a str) -> Answered<'a, Vec<String>> {
        Box::pin(async move {
            settle_value(
                self.request("workspace.list_dir", json!({"path": path}))
                    .await,
                decode_json,
            )
        })
    }

    fn walk_files<'a>(&'a self, base: &'a str, max: usize) -> Answered<'a, Vec<String>> {
        Box::pin(async move {
            settle_value(
                self.request("workspace.walk_files", json!({"base": base, "max": max}))
                    .await,
                decode_json,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_system::net::PrivateNetAllow;
    use flux_system::net::{BindExposure, DialTarget, InboundLimits};
    use flux_system::port::{GuardedNetwork, GuardedProcess, GuardedWorkspaceFiles};
    use flux_system::Workspace;
    use rcgen::{generate_simple_self_signed, CertifiedKey};

    /// C-654, acceptance 1: `host.metrics` joins the bounded wire vocabulary, and it arrives under
    /// a protocol version bump rather than as an implicit extension of the shipped one.
    #[test]
    fn host_metrics_joins_the_bounded_vocabulary_under_a_protocol_version_bump() {
        assert!(
            bounded_operations().contains(&"host.metrics"),
            "the bounded vocabulary must carry the metrics read: {:?}",
            bounded_operations()
        );
        // A compile-time pin: adding a wire operation is a versioned protocol change (Decision
        // 0018 rule 6), not an implicit extension, and v2 is the version that shipped without one.
        const { assert!(PROTOCOL_VERSION > 2) };
    }

    /// A self-signed `localhost` pair: the PEM a client must trust, and the server's TLS config.
    async fn localhost_tls() -> (String, axum_server::tls_rustls::RustlsConfig) {
        ensure_crypto_provider();
        let CertifiedKey { cert, signing_key } =
            generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let cert_pem = cert.pem();
        let tls = axum_server::tls_rustls::RustlsConfig::from_pem(
            cert_pem.as_bytes().to_vec(),
            signing_key.serialize_pem().into_bytes(),
        )
        .await
        .unwrap();
        (cert_pem, tls)
    }

    /// Serve a router over TLS on a fresh loopback port; abort the handle to stop it. The router is
    /// built *from* the bound address, because the production one validates the address it binds.
    fn serve_on_loopback(
        build: impl FnOnce(SocketAddr) -> Router,
        tls: axum_server::tls_rustls::RustlsConfig,
    ) -> (SocketAddr, tokio::task::JoinHandle<std::io::Result<()>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let app = build(address);
        let handle = tokio::spawn(async move {
            axum_server::from_tcp_rustls(listener, tls)
                .unwrap()
                .serve(app.into_make_service())
                .await
        });
        (address, handle)
    }

    /// A peer that answers **only** the handshake, with whatever frame the test hands it. This is
    /// how a differently-versioned or differently-capable far side is represented: the shipped
    /// router always speaks the current version, so it cannot stand in for one that does not.
    fn peer_announcing(handshake: Value) -> Router {
        Router::new().route(
            "/system/v1/handshake",
            get(move || {
                let handshake = handshake.clone();
                async move { Json(handshake) }
            }),
        )
    }

    fn workspace_system(tag: &str) -> (std::path::PathBuf, Arc<System>) {
        let root = std::env::temp_dir().join(format!(
            "flux-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let system = Arc::new(System::new(Workspace::new(&root).unwrap()));
        (root, system)
    }

    /// C-654, acceptance 1: the version bump is a *negotiation*, and it refuses a mixed pair from
    /// both seats — the client refuses to install an older peer, and the server refuses an older
    /// caller's frame. Neither direction degrades to a partial vocabulary: a shared operation name
    /// is not a shared meaning, so pairing has to fail where an operator can see it rather than
    /// inside whichever effect first hits the disagreement.
    #[tokio::test]
    async fn the_protocol_refuses_a_mixed_version_pair_from_both_seats() {
        let (cert_pem, tls) = localhost_tls().await;

        // Direction one: this build meets a peer one version behind.
        let (address, older_peer) = serve_on_loopback(
            |_| {
                peer_announcing(json!({
                    "protocol_version": PROTOCOL_VERSION - 1,
                    "substrate_kind": "native",
                    "workspace": "/srv/work",
                    "confinement": "none",
                    "operations": ["workspace.read_bytes"],
                }))
            },
            tls,
        );
        let message = match HttpDelegate::connect_with_ca_pem(
            &format!("https://localhost:{}", address.port()),
            "test-token".into(),
            &PrivateNetAllow::from_hosts(["localhost".into()]),
            cert_pem.as_bytes(),
        )
        .await
        {
            Ok(_) => panic!("an older peer must not pair with this build"),
            Err(error) => error.to_string(),
        };
        assert!(
            message.contains("protocol mismatch")
                && message.contains(&PROTOCOL_VERSION.to_string())
                && message.contains(&(PROTOCOL_VERSION - 1).to_string()),
            "the refusal must name both versions so an operator knows which side to move: {message}"
        );
        older_peer.abort();

        // Direction two: this build serves, and an older caller's frame arrives.
        let (root, system) = workspace_system("remote-version-server");
        let (cert_pem, tls) = localhost_tls().await;
        let (address, server) = serve_on_loopback(
            |bind| {
                remote_system_router(
                    system,
                    ServerAuth::from_token(Some("test-token".into())),
                    bind,
                )
                .unwrap()
            },
            tls,
        );
        let client = reqwest::Client::builder()
            .add_root_certificate(reqwest::Certificate::from_pem(cert_pem.as_bytes()).unwrap())
            .resolve("localhost", address)
            .build()
            .unwrap();
        let arguments = json!({});
        let response = client
            .post(format!(
                "https://localhost:{}/system/v1/execute",
                address.port()
            ))
            .bearer_auth("test-token")
            .json(&json!({
                "protocol_version": PROTOCOL_VERSION - 1,
                "operation_id": "older-caller-1",
                "fingerprint": fingerprint("host.metrics", &arguments),
                "operation": "host.metrics",
                "arguments": arguments,
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "an older caller is refused before the operation is dispatched"
        );
        let answer = response.json::<WireAnswer>().await.unwrap();
        assert!(matches!(answer.status, WireStatus::Refused));
        assert!(
            answer
                .detail
                .as_deref()
                .unwrap_or_default()
                .contains("unsupported remote-system protocol version"),
            "{answer:?}"
        );

        server.abort();
        std::fs::remove_dir_all(root).ok();
    }

    /// C-654, acceptances 1 and 4: readings travel the bounded operation and arrive as typed
    /// answers stamped `remotely_reported` — the single-kind face and the one-round-trip snapshot
    /// alike. An instrument the serving machine lacks stays explicitly unavailable across the hop
    /// rather than collapsing into a zero a projection would read as a measurement.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn host_metrics_travel_the_bounded_wire_and_arrive_remotely_reported() {
        use flux_system::metrics::MetricUnavailable;

        let (root, system) = workspace_system("remote-metrics");
        let (cert_pem, tls) = localhost_tls().await;
        let (address, server) = serve_on_loopback(
            |bind| {
                remote_system_router(
                    system,
                    ServerAuth::from_token(Some("test-token".into())),
                    bind,
                )
                .unwrap()
            },
            tls,
        );

        let (delegate, handshake) = HttpDelegate::connect_with_ca_pem(
            &format!("https://localhost:{}", address.port()),
            "test-token".into(),
            &PrivateNetAllow::from_hosts(["localhost".into()]),
            cert_pem.as_bytes(),
        )
        .await
        .unwrap();
        assert!(
            handshake.operations.iter().any(|op| op == "host.metrics"),
            "the peer advertises the bounded metrics operation: {:?}",
            handshake.operations
        );
        assert!(
            handshake.metric_kinds.iter().any(|kind| kind == "cpu"),
            "the handshake declares the vocabulary the peer measures: {:?}",
            handshake.metric_kinds
        );
        let remote = RemoteSystem::identified(delegate, handshake.identity());

        let answers = GuardedMetrics::read_metrics(&remote)
            .await
            .expect("a v3 peer serves the metrics family");
        let kinds: Vec<MetricKind> = answers.iter().map(MetricAnswer::kind).collect();
        assert_eq!(
            kinds,
            MetricKind::ALL.to_vec(),
            "one round trip returns the whole snapshot in canonical order"
        );
        let mut served = 0;
        for answer in &answers {
            match answer {
                MetricAnswer::Served(snapshot) => {
                    served += 1;
                    assert!(
                        snapshot.remotely_reported,
                        "{} crossed the wire and must not claim local observation",
                        snapshot.kind()
                    );
                }
                MetricAnswer::Unavailable { reason, .. } => {
                    // The other negative survives the hop with its own reason rather than becoming
                    // a served zero.
                    assert!(matches!(
                        reason,
                        MetricUnavailable::NoInstrument
                            | MetricUnavailable::ReadFailed
                            | MetricUnavailable::UnsupportedPlatform
                    ));
                }
            }
        }
        assert!(served > 0, "nothing was measured at all: {answers:?}");

        // The single-kind face asks for, and gets, exactly the instrument named.
        let memory = GuardedMetrics::read_metric(&remote, MetricKind::Memory)
            .await
            .expect("memory is served");
        let snapshot = memory.served().expect("a machine has physical memory");
        assert_eq!(snapshot.kind(), MetricKind::Memory);
        assert!(snapshot.remotely_reported);
        match &snapshot.reading {
            MetricReading::Memory(pool) => {
                assert!(pool.total_bytes > 0, "bytes survive the wire: {pool:?}");
                assert!(pool.used_bytes <= pool.total_bytes);
            }
            other => panic!("memory answered {other:?}"),
        }

        server.abort();
        std::fs::remove_dir_all(root).ok();
    }

    /// C-654, acceptance 1: a peer that does not serve the family answers the **typed** unsupported
    /// the port already has, never a decode error and never a fabricated zero.
    ///
    /// A peer declares its metric vocabulary in the handshake, so a build without the family (an
    /// older one has no such field at all) declares nothing — and the mode matters more than the
    /// message: `Unserved` tells a caller that retrying will never help, which is the one thing an
    /// operator can act on.
    #[tokio::test]
    async fn a_peer_that_serves_no_metrics_answers_a_typed_unserved() {
        let (cert_pem, tls) = localhost_tls().await;
        let (address, peer) = serve_on_loopback(
            |_| {
                peer_announcing(json!({
                    "protocol_version": PROTOCOL_VERSION,
                    "substrate_kind": "native",
                    "workspace": "/srv/work",
                    "confinement": "none",
                    // The vocabulary this build shipped *before* the metrics family: same version,
                    // no metric operation, and no `metric_kinds` field to decode.
                    "operations": ["workspace.read_bytes", "process.run"],
                }))
            },
            tls,
        );

        let (delegate, handshake) = HttpDelegate::connect_with_ca_pem(
            &format!("https://localhost:{}", address.port()),
            "test-token".into(),
            &PrivateNetAllow::from_hosts(["localhost".into()]),
            cert_pem.as_bytes(),
        )
        .await
        .map_err(|error| error.to_string())
        .expect("a same-version peer pairs even when it serves fewer families");
        assert!(handshake.metric_kinds.is_empty(), "{handshake:?}");
        let remote = RemoteSystem::identified(delegate, handshake.identity());

        assert!(
            GuardedMetrics::served_metric_kinds(&remote).is_empty(),
            "a peer that declared nothing must not be credited with a vocabulary"
        );
        for error in [
            GuardedMetrics::read_metrics(&remote)
                .await
                .expect_err("the snapshot face fails closed"),
            GuardedMetrics::read_metric(&remote, MetricKind::CpuUsage)
                .await
                .expect_err("the single-kind face fails closed"),
        ] {
            assert_eq!(
                flux_system::remote::failure_mode(&error),
                Some(flux_system::remote::FailureMode::Unserved),
                "retrying never helps, so the mode must be unserved rather than refused or \
                 unreachable: {error}"
            );
        }

        peer.abort();
    }

    /// C-654: the decoder re-imposes the vocabulary's bounds on what the wire claims.
    ///
    /// The caps on labels, mounts and sensors are a construction-site convention over public
    /// fields, not a type invariant — so a peer that reports forty mounts with megabyte names is
    /// representable, and the only place to stop it is where its bytes become typed values. The
    /// list is capped before the entries are built, not after, or the allocation has already
    /// happened by the time anything is truncated.
    #[test]
    fn the_metric_decoder_rebounds_everything_the_wire_claims() {
        let long = "q".repeat(9000);
        let mounts: Vec<Value> = (0..(MAX_MOUNTS + 64))
            .map(|index| {
                json!({
                    "mount_point": format!("/mnt/{long}{index}"),
                    "filesystem": format!("ext4{long}"),
                    "total_bytes": 4096,
                    "available_bytes": 1024,
                    "used_bytes": 3072,
                })
            })
            .collect();
        let sensors: Vec<Value> = (0..(MAX_SENSORS + 64))
            .map(|_| json!({"label": format!("chip\u{1b}[2J/{long}"), "celsius": 41.5}))
            .collect();
        let fans: Vec<Value> = (0..(MAX_SENSORS + 64))
            .map(|_| json!({"label": long.clone(), "rpm": 900}))
            .collect();

        let decoded = decode_metric_answers(json!({"answers": [
            {"kind": "disk", "status": "served", "sampled_at_ms": 1, "reading": mounts},
            {"kind": "temperature", "status": "served", "sampled_at_ms": 1, "reading": sensors},
            {"kind": "fan", "status": "served", "sampled_at_ms": 1, "reading": fans},
        ]}))
        .expect("an oversized but well-formed frame decodes, bounded");

        for answer in &decoded {
            let snapshot = answer.served().expect("all three are served");
            assert!(
                !snapshot.remotely_reported,
                "the transport does not get to assert provenance; the hop does"
            );
            match &snapshot.reading {
                MetricReading::Disk(mounts) => {
                    assert_eq!(mounts.len(), MAX_MOUNTS);
                    for mount in mounts {
                        assert!(mount.mount_point.len() <= 64, "{mount:?}");
                        assert!(mount.filesystem.len() <= 64, "{mount:?}");
                    }
                }
                MetricReading::Temperature(sensors) => {
                    assert_eq!(sensors.len(), MAX_SENSORS);
                    for sensor in sensors {
                        assert!(sensor.label.len() <= 64);
                        assert!(
                            !sensor.label.chars().any(char::is_control),
                            "a control sequence reached an operator's terminal: {:?}",
                            sensor.label
                        );
                    }
                }
                MetricReading::FanSpeed(sensors) => {
                    assert_eq!(sensors.len(), MAX_SENSORS);
                    assert!(sensors.iter().all(|sensor| sensor.label.len() <= 64));
                }
                other => panic!("unexpected reading {other:?}"),
            }
        }

        // The answer list itself is bounded by the closed vocabulary, so a peer cannot make a
        // caller iterate over more answers than there are kinds.
        let flood: Vec<Value> = (0..500)
            .map(|_| json!({"kind": "uptime", "status": "unavailable", "reason": "no_instrument"}))
            .collect();
        assert_eq!(
            decode_metric_answers(json!({"answers": flood}))
                .unwrap()
                .len(),
            MetricKind::ALL.len()
        );

        // A kind or a status this build cannot read is refused, never guessed at.
        for hostile in [
            json!({"answers": [{"kind": "gpu", "status": "served", "sampled_at_ms": 1, "reading": {}}]}),
            json!({"answers": [{"kind": "cpu", "status": "estimated"}]}),
            json!({"answers": [{"kind": "swap", "status": "unavailable"}]}),
        ] {
            assert!(
                decode_metric_answers(hostile.clone()).is_err(),
                "decoded a frame it should have refused: {hostile}"
            );
        }
    }

    #[tokio::test]
    async fn delivery_ids_dedupe_and_restart_as_an_honest_unknown_without_persisting_arguments() {
        let root = std::env::temp_dir().join(format!(
            "flux-remote-delivery-{}-{}",
            std::process::id(),
            now_millis().unwrap()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let system = Arc::new(System::new(Workspace::new(&root).unwrap()));
        let arguments = json!({"path":"marker.txt", "contents":"SECRET-MUST-NOT-PERSIST"});
        let request = WireRequest {
            protocol_version: PROTOCOL_VERSION,
            operation_id: "caller-op-1".into(),
            fingerprint: fingerprint("workspace.write_bytes", &arguments),
            operation: "workspace.write_bytes".into(),
            arguments,
        };
        let ledger = DeliveryLedger::new(system.clone());

        assert!(matches!(
            ledger.claim(&request).await.unwrap(),
            DeliveryClaim::Execute
        ));
        assert!(matches!(
            ledger.claim(&request).await.unwrap(),
            DeliveryClaim::Unknown(_)
        ));
        ledger
            .finish("caller-op-1", served(json!({"ok": true})))
            .await
            .unwrap();
        assert!(matches!(
            ledger.claim(&request).await.unwrap(),
            DeliveryClaim::Cached(_)
        ));

        let mut collision = request.clone();
        collision.fingerprint = "different".into();
        assert!(matches!(
            ledger.claim(&collision).await.unwrap(),
            DeliveryClaim::Refused(_)
        ));
        let persisted = system.read_file(DELIVERY_LEDGER_PATH).await.unwrap();
        assert!(!persisted.contains("SECRET-MUST-NOT-PERSIST"));

        let restarted = DeliveryLedger::new(system);
        assert!(matches!(
            restarted.claim(&request).await.unwrap(),
            DeliveryClaim::Unknown(_)
        ));
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn https_loopback_drives_workspace_and_process_operations_over_real_bytes() {
        ensure_crypto_provider();
        let root = std::env::temp_dir().join(format!(
            "flux-remote-system-https-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("marker.txt"), "remote").unwrap();
        let system = Arc::new(System::new(Workspace::new(&root).unwrap()));

        let CertifiedKey { cert, signing_key } =
            generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let cert_pem = cert.pem();
        let tls = axum_server::tls_rustls::RustlsConfig::from_pem(
            cert_pem.as_bytes().to_vec(),
            signing_key.serialize_pem().into_bytes(),
        )
        .await
        .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let app = remote_system_router(
            system,
            ServerAuth::from_token(Some("test-token".into())),
            address,
        )
        .unwrap();
        let server = tokio::spawn(async move {
            axum_server::from_tcp_rustls(listener, tls)
                .unwrap()
                .serve(app.into_make_service())
                .await
        });

        let (delegate, handshake) = HttpDelegate::connect_with_ca_pem(
            &format!("https://localhost:{}", address.port()),
            "test-token".into(),
            &PrivateNetAllow::from_hosts(["localhost".into()]),
            cert_pem.as_bytes(),
        )
        .await
        .unwrap();
        let remote = RemoteSystem::identified(delegate, handshake.identity());

        assert_eq!(remote.read_file("marker.txt").await.unwrap(), "remote");
        remote
            .write_file("written.txt", "through https")
            .await
            .unwrap();
        assert_eq!(
            remote.read_file("written.txt").await.unwrap(),
            "through https"
        );
        let output = remote
            .run(&["pwd".into()], Duration::from_secs(10))
            .await
            .unwrap();
        assert_eq!(
            std::path::Path::new(output.stdout.trim()),
            root.canonicalize().unwrap()
        );
        let mut child = remote
            .spawn_background(
                &["sh".into(), "-c".into(), "printf managed-over-wss".into()],
                &[],
            )
            .await
            .unwrap();
        let mut managed_output = String::new();
        for _ in 0..200 {
            managed_output.push_str(&child.read_output().0);
            if managed_output == "managed-over-wss" && !child.status().running {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(managed_output, "managed-over-wss");
        assert!(!child.status().running);

        let echo_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let echo_address = echo_listener.local_addr().unwrap();
        let echo = tokio::spawn(async move {
            let (mut stream, _) = echo_listener.accept().await.unwrap();
            let mut bytes = [0; 4];
            tokio::io::AsyncReadExt::read_exact(&mut stream, &mut bytes)
                .await
                .unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut stream, &bytes)
                .await
                .unwrap();
        });
        let private = PrivateNetAllow::from_hosts(["127.0.0.1".into()]);
        let refused = match remote
            .dial_scoped(
                &DialTarget::Tcp {
                    host: "127.0.0.1".into(),
                    port: echo_address.port(),
                },
                &PrivateNetAllow::None,
            )
            .await
        {
            Ok(_) => panic!("private dial unexpectedly succeeded without a grant"),
            Err(error) => error,
        };
        assert_eq!(
            flux_system::remote::failure_mode(&refused),
            Some(flux_system::remote::FailureMode::Refused)
        );
        let mut remote_stream = remote
            .dial_scoped(
                &DialTarget::Tcp {
                    host: "127.0.0.1".into(),
                    port: echo_address.port(),
                },
                &private,
            )
            .await
            .unwrap();
        remote_stream.write_all(b"echo").await.unwrap();
        assert_eq!(remote_stream.read(4).await.unwrap(), b"echo");
        echo.await.unwrap();

        let limits = InboundLimits {
            max_connections: 1,
            max_frame_bytes: 64,
            io_timeout: Duration::from_secs(5),
        };
        let mut remote_listener = remote
            .bind_tcp(
                "127.0.0.1:0".parse().unwrap(),
                BindExposure::LoopbackOnly,
                limits,
            )
            .await
            .unwrap();
        let listener_address = remote_listener.local_addr().unwrap();
        let client = tokio::spawn(async move {
            let mut stream = tokio::net::TcpStream::connect(listener_address)
                .await
                .unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut stream, &[b'x'; 256])
                .await
                .unwrap();
            let mut response = [0; 8];
            tokio::time::timeout(
                Duration::from_secs(1),
                tokio::io::AsyncReadExt::read_exact(&mut stream, &mut response),
            )
            .await
            .expect("remote outbound half must progress while inbound is backpressured")
            .unwrap();
            response
        });
        let (accepted, _) = remote_listener.accept().await.unwrap();
        let mut protocol = accepted.into_async_io(limits.max_frame_bytes);
        tokio::time::sleep(Duration::from_millis(25)).await;
        tokio::io::AsyncWriteExt::write_all(&mut protocol, b"response")
            .await
            .unwrap();
        assert_eq!(&client.await.unwrap(), b"response");
        drop(protocol);
        let second_client = tokio::spawn(tokio::net::TcpStream::connect(listener_address));
        let second_accepted =
            tokio::time::timeout(Duration::from_secs(1), remote_listener.accept())
                .await
                .expect("dropping remote protocol IO must release remote admission")
                .unwrap();
        drop(second_accepted);
        second_client.await.unwrap().unwrap();

        let mut remote_udp = remote
            .bind_udp(
                "127.0.0.1:0".parse().unwrap(),
                BindExposure::LoopbackOnly,
                limits,
                private,
            )
            .await
            .unwrap();
        let udp_peer = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        udp_peer
            .send_to(b"datagram", remote_udp.local_addr().unwrap())
            .await
            .unwrap();
        let (datagram, udp_peer_address) = remote_udp.recv_from().await.unwrap();
        assert_eq!(datagram, b"datagram");
        remote_udp
            .send_to(b"reply", "127.0.0.1", udp_peer_address.port())
            .await
            .unwrap();
        let mut udp_reply = [0; 8];
        let count = udp_peer.recv(&mut udp_reply).await.unwrap();
        assert_eq!(&udp_reply[..count], b"reply");
        assert!(remote.substrate_identity().remotely_reported);

        server.abort();
        let _ = server.await;
        std::fs::remove_dir_all(root).ok();
    }
}
