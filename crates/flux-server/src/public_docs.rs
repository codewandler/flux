//! Release-matched documentation and Flux-Lang workbench server.
//!
//! Parsing/projection is always available. Guarded execution and LSP are a separate router mounted
//! only for a loopback listener: a public bind never constructs an executor or scratch workspace.

use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{DefaultBodyLimit, Path, State, WebSocketUpgrade};
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use futures::{SinkExt as _, StreamExt as _};
use rand::RngExt as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tower_http::timeout::TimeoutLayer;

use flux_provider::Provider;
use flux_runtime::{ApprovalChoice, ApprovalQueue, RemoteApprover};
use flux_system::ScratchWorkspace;

const SITE_ZIP: &[u8] = include_bytes!("../assets/public-docs.zip");
const MAX_SOURCE_BODY_BYTES: usize = 256 * 1024;
const SESSION_COOKIE: &str = "flux_docs_session";

#[derive(Clone)]
struct Asset {
    bytes: Bytes,
    content_type: &'static str,
    immutable: bool,
}

#[derive(Clone)]
struct DocsState {
    assets: Arc<HashMap<String, Asset>>,
    version: &'static str,
    runtime: Option<Arc<WorkbenchRuntime>>,
}

struct WorkbenchRuntime {
    launch_secret: String,
    exchanged: std::sync::atomic::AtomicBool,
    model: String,
    provider: Arc<dyn Provider>,
    browser_sessions: Mutex<HashMap<String, Arc<BrowserSession>>>,
    workbenches: Mutex<HashMap<String, Arc<WorkbenchSession>>>,
}

struct BrowserSession {
    id: String,
    scratch: ScratchWorkspace,
}

struct WorkbenchSession {
    owner: String,
    fixture: &'static Fixture,
    scratch: ScratchWorkspace,
    flow: Option<FlowProgram>,
    app: Option<Arc<flux_app::App>>,
    approvals: Arc<ApprovalQueue>,
    run: tokio::sync::Mutex<RunState>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    events: Arc<Mutex<Vec<Value>>>,
}

struct WorkbenchSink {
    events: Arc<Mutex<Vec<Value>>>,
}

impl flux_flow::AgentSink for WorkbenchSink {
    fn text_delta(&mut self, text: &str) {
        self.push(json!({"kind": "text", "text": text}));
    }

    fn tool_call(&mut self, name: &str, input: &Value) {
        self.push(json!({"kind": "tool_call", "name": name, "input": input}));
    }

    fn tool_result(&mut self, name: &str, result: &flux_runtime::ToolResult) {
        self.push(json!({
            "kind": "tool_result",
            "name": name,
            "content": result.content,
            "is_error": result.is_error,
        }));
    }
}

impl WorkbenchSink {
    fn push(&self, event: Value) {
        let mut events = self
            .events
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if events.len() < 2_000 {
            events.push(event);
        }
    }
}

struct FlowProgram {
    client: Arc<flux_sdk::FlowClient>,
    ast: flux_lang::ast::DraftAst,
    inputs: serde_json::Map<String, Value>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
enum RunStatus {
    Ready,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

struct RunState {
    status: RunStatus,
    result: Option<String>,
    error: Option<String>,
    tool_calls: Vec<String>,
}

struct Fixture {
    id: &'static str,
    kind: FixtureKind,
    allowed_ops: &'static [&'static str],
    allowed_files: &'static [&'static str],
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FixtureKind {
    Flow,
    App,
}

const FIXTURES: &[Fixture] = &[
    Fixture {
        id: "summarize-readme",
        kind: FixtureKind::Flow,
        allowed_ops: &["read", "ai.reason"],
        allowed_files: &["README.md"],
    },
    Fixture {
        id: "latest-release",
        kind: FixtureKind::Flow,
        allowed_ops: &["web.fetch"],
        allowed_files: &[],
    },
    Fixture {
        id: "cached-page",
        kind: FixtureKind::Flow,
        allowed_ops: &["read", "web.fetch"],
        allowed_files: &["cache/page.html"],
    },
    Fixture {
        id: "wait-for-artifact",
        kind: FixtureKind::Flow,
        allowed_ops: &["path_exists"],
        allowed_files: &["target/release/flux"],
    },
    Fixture {
        id: "rust-files",
        kind: FixtureKind::Flow,
        allowed_ops: &["glob", "file_stat"],
        allowed_files: &["src/main.rs", "src/lib.rs", "tests/smoke.rs"],
    },
    Fixture {
        id: "first-app-a",
        kind: FixtureKind::App,
        allowed_ops: &["search", "send"],
        allowed_files: &["docs/product.md", "docs/policies.md"],
    },
    Fixture {
        id: "first-app-b",
        kind: FixtureKind::App,
        allowed_ops: &["search", "ai.reason", "send"],
        allowed_files: &["docs/product.md", "docs/policies.md"],
    },
];

#[derive(Serialize)]
struct VersionResponse {
    version: &'static str,
}

#[derive(Deserialize)]
struct ProjectionRequest {
    source: String,
}

#[derive(Deserialize)]
struct ExchangeRequest {
    secret: String,
}

#[derive(Deserialize)]
struct CreateSessionRequest {
    fixture: String,
    source: String,
    #[serde(default)]
    files: HashMap<String, String>,
    #[serde(default)]
    inputs: serde_json::Map<String, Value>,
}

#[derive(Deserialize)]
struct DecisionRequest {
    fingerprint: String,
    choice: String,
}

#[derive(Deserialize)]
struct AppInputRequest {
    text: String,
}

#[derive(Serialize)]
struct BootstrapResponse {
    version: &'static str,
    execution: bool,
    lsp: bool,
    model: Option<String>,
    runnable_fixtures: Vec<&'static str>,
}

/// Build the environment-independent public documentation app for a distributed CLI version.
pub fn docs_app(version: &'static str) -> anyhow::Result<Router> {
    let state = DocsState {
        assets: Arc::new(load_assets()?),
        version,
        runtime: None,
    };
    public_router(state)
}

fn public_router(state: DocsState) -> anyhow::Result<Router> {
    Ok(Router::new()
        .route("/version", get(version_response))
        .route("/api/playground/project", post(project_source))
        .route("/", get(|| async { Redirect::permanent("/flux/") }))
        .route("/flux", get(|| async { Redirect::permanent("/flux/") }))
        .route(
            "/console",
            get(|| async { Redirect::permanent("/console/") }),
        )
        .fallback(get(static_asset))
        .layer(DefaultBodyLimit::max(MAX_SOURCE_BODY_BYTES))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(5),
        ))
        .with_state(state))
}

/// Build the docs router for `bind`. Runtime state and routes exist only on loopback.
pub fn docs_app_for_bind(
    version: &'static str,
    bind: SocketAddr,
    provider: Arc<dyn Provider>,
    model: String,
) -> anyhow::Result<(Router, Option<String>)> {
    if !bind.ip().is_loopback() {
        return Ok((docs_app(version)?, None));
    }
    let launch_secret = random_token();
    let runtime = Arc::new(WorkbenchRuntime {
        launch_secret: launch_secret.clone(),
        exchanged: std::sync::atomic::AtomicBool::new(false),
        model,
        provider,
        browser_sessions: Mutex::new(HashMap::new()),
        workbenches: Mutex::new(HashMap::new()),
    });
    let state = DocsState {
        assets: Arc::new(load_assets()?),
        version,
        runtime: Some(runtime),
    };
    let app = public_router(state.clone())?.merge(
        Router::new()
            .route("/api/workbench/exchange", post(exchange_secret))
            .route("/api/workbench/bootstrap", get(bootstrap))
            .route("/api/workbench/lsp", get(browser_lsp_upgrade))
            .route("/api/workbench/sessions", post(create_session))
            .route(
                "/api/workbench/sessions/{id}",
                get(session_status).delete(delete_session),
            )
            .route("/api/workbench/sessions/{id}/run", post(run_session))
            .route(
                "/api/workbench/sessions/{id}/input",
                post(deliver_app_input),
            )
            .route("/api/workbench/sessions/{id}/cancel", post(cancel_session))
            .route(
                "/api/workbench/sessions/{id}/approvals/{approval}",
                post(decide_approval),
            )
            .route("/api/workbench/sessions/{id}/lsp", get(lsp_upgrade))
            .with_state(state),
    );
    Ok((app, Some(launch_secret)))
}

/// Bind and serve the release-matched docs until Ctrl-C/SIGTERM.
pub async fn serve(
    bind: SocketAddr,
    version: &'static str,
    provider: Arc<dyn Provider>,
    model: String,
) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let addr = listener.local_addr()?;
    let (app, launch_secret) = docs_app_for_bind(version, addr, provider, model)?;
    let fragment = launch_secret
        .as_deref()
        .map(|secret| format!("#flux-launch={secret}"))
        .unwrap_or_default();
    eprintln!("Serving flux v{version} documentation at http://{addr}/flux/{fragment}");
    eprintln!("  Flux-Lang console: http://{addr}/console/{fragment}");
    if launch_secret.is_none() {
        eprintln!("  Public bind: execution, scratch sessions, approvals, and LSP are disabled");
    }
    axum::serve(listener, app)
        .with_graceful_shutdown(crate::shutdown_signal())
        .await?;
    Ok(())
}

async fn version_response(State(state): State<DocsState>) -> Json<VersionResponse> {
    Json(VersionResponse {
        version: state.version,
    })
}

async fn project_source(Json(request): Json<ProjectionRequest>) -> Response {
    match flux_lang::editor::project_source(&request.source, None) {
        Ok(projection) => Json(projection).into_response(),
        Err(flow_error) => match flux_lang::program::Module::parse_str(&request.source) {
            Ok(flux_lang::program::Module::Program(_)) => Json(json!({
                "graph": null,
                "diagnostics": [{
                    "code": "program_source_only",
                    "message": "App programs use source/LSP mode; graph projection is per journey."
                }]
            }))
            .into_response(),
            _ => (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({"error": flow_error.to_string()})),
            )
                .into_response(),
        },
    }
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn same_origin(headers: &HeaderMap) -> bool {
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|origin| origin == format!("http://{host}"))
}

fn secret_matches(expected: &str, supplied: &str) -> bool {
    use sha2::{Digest as _, Sha256};
    let expected = Sha256::digest(expected.as_bytes());
    let supplied = Sha256::digest(supplied.as_bytes());
    expected
        .iter()
        .zip(supplied.iter())
        .fold(0u8, |diff, (left, right)| diff | (left ^ right))
        == 0
}

fn cookie_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .map(str::trim)
        .find_map(|part| part.strip_prefix(&format!("{SESSION_COOKIE}=")))
}

fn authenticated_owner(state: &DocsState, headers: &HeaderMap) -> Option<String> {
    let runtime = state.runtime.as_ref()?;
    let token = cookie_token(headers)?;
    runtime
        .browser_sessions
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(token)
        .map(|session| session.id.clone())
}

fn owned_session(
    state: &DocsState,
    headers: &HeaderMap,
    id: &str,
) -> Option<Arc<WorkbenchSession>> {
    let owner = authenticated_owner(state, headers)?;
    state
        .runtime
        .as_ref()?
        .workbenches
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(id)
        .filter(|session| session.owner == owner)
        .cloned()
}

async fn exchange_secret(
    State(state): State<DocsState>,
    headers: HeaderMap,
    Json(request): Json<ExchangeRequest>,
) -> Response {
    if !same_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(runtime) = &state.runtime else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !secret_matches(&runtime.launch_secret, &request.secret)
        || runtime
            .exchanged
            .swap(true, std::sync::atomic::Ordering::AcqRel)
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let token = random_token();
    let scratch = match ScratchWorkspace::new() {
        Ok(scratch) => scratch,
        Err(error) => return internal_error(error),
    };
    let owner = Arc::new(BrowserSession {
        id: random_token(),
        scratch,
    });
    runtime
        .browser_sessions
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(token.clone(), owner);
    let mut response = Json(json!({"ok": true})).into_response();
    let cookie = format!("{SESSION_COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/");
    response.headers_mut().insert(
        header::SET_COOKIE,
        header::HeaderValue::from_str(&cookie).expect("random token is a legal cookie value"),
    );
    response
}

async fn bootstrap(State(state): State<DocsState>, headers: HeaderMap) -> Response {
    let authenticated = authenticated_owner(&state, &headers).is_some();
    let runtime = state.runtime.as_ref().filter(|_| authenticated);
    Json(BootstrapResponse {
        version: state.version,
        execution: runtime.is_some(),
        lsp: runtime.is_some(),
        model: runtime.map(|runtime| runtime.model.clone()),
        runnable_fixtures: if runtime.is_some() {
            FIXTURES.iter().map(|fixture| fixture.id).collect()
        } else {
            Vec::new()
        },
    })
    .into_response()
}

async fn create_session(
    State(state): State<DocsState>,
    headers: HeaderMap,
    Json(request): Json<CreateSessionRequest>,
) -> Response {
    if !same_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(owner) = authenticated_owner(&state, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(runtime) = &state.runtime else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(fixture) = FIXTURES
        .iter()
        .find(|fixture| fixture.id == request.fixture)
    else {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "this code block is edit/check-only"})),
        )
            .into_response();
    };
    if let Some(path) = request
        .files
        .keys()
        .find(|path| !fixture.allowed_files.contains(&path.as_str()))
    {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": format!("fixture file {path:?} is not declared")})),
        )
            .into_response();
    }

    let scratch = match ScratchWorkspace::new() {
        Ok(scratch) => scratch,
        Err(error) => return internal_error(error),
    };
    if let Err(error) = scratch
        .system()
        .write_file("main.flux", &request.source)
        .await
    {
        return internal_error(error);
    }
    for (path, contents) in &request.files {
        if let Err(error) = scratch.system().write_file(path, contents).await {
            return internal_error(error);
        }
    }

    let approvals = Arc::new(ApprovalQueue::new(Duration::from_secs(120)));
    let (flow, app, risk_summary, risk_ops, destructive, mutating) = match fixture.kind {
        FixtureKind::Flow => {
            let mut client = match flux_sdk::FlowClient::builder()
                .model(runtime.model.clone())
                .approver(Arc::new(RemoteApprover::new(approvals.clone())))
                .build(runtime.provider.clone(), scratch.root().to_path_buf())
            {
                Ok(client) => client,
                Err(error) => return internal_error(error),
            };
            if fixture.allowed_ops.contains(&"web.fetch") {
                if let Err(error) = client.try_register_pack(|registry| {
                    flux_web::try_register_web(registry, &flux_web::WebOptions::default())
                }) {
                    return internal_error(error);
                }
            }
            let ast = match client.parse(&request.source) {
                Ok(ast) => ast,
                Err(error) => return invalid_source(error),
            };
            let risk = flux_flow::runtime::plan_risk(&ast, client.registry());
            if let Some(response) = forbidden_op(fixture, &risk.ops) {
                return response;
            }
            if let Err(diagnostics) = client.analyze_seeded(&ast, request.inputs.keys().cloned()) {
                return invalid_source(format!("{diagnostics:?}"));
            }
            (
                Some(FlowProgram {
                    client: Arc::new(client),
                    ast,
                    inputs: request.inputs,
                }),
                None,
                risk.summary(),
                risk.ops,
                risk.destructive,
                risk.mutating,
            )
        }
        FixtureKind::App => {
            let module = match flux_lang::program::Module::parse_str(&request.source) {
                Ok(flux_lang::program::Module::Program(program)) => program,
                Ok(flux_lang::program::Module::Flow(_)) => {
                    return invalid_source("this fixture requires a Flux app program")
                }
                Err(error) => return invalid_source(error),
            };
            if let Some(op) = module
                .agents
                .iter()
                .flat_map(|agent| agent.tools.iter())
                .find(|op| !fixture.allowed_ops.contains(&op.as_str()))
            {
                return forbidden_op_response(fixture, op);
            }
            let backend: Arc<dyn flux_capabilities::DatasourceBackend> =
                Arc::new(flux_capabilities::MemoryBackend::new());
            let docs: Vec<(String, String)> = request
                .files
                .iter()
                .filter(|(path, _)| path.ends_with(".md"))
                .map(|(path, text)| (path.clone(), text.clone()))
                .collect();
            if let Err(error) = flux_capabilities::ingest_markdown(&*backend, "handbook", &docs) {
                return internal_error(error);
            }
            let mut registry = flux_runtime::ToolRegistry::new();
            if let Err(error) =
                flux_capabilities::try_register_datasource_ops(&mut registry, backend)
            {
                return internal_error(error);
            }
            let environment = flux_runtime::ExecutionEnvironment::new(
                Arc::new(scratch.system().clone()),
                registry,
                flux_runtime::PermissionManager::new(),
                Arc::new(RemoteApprover::new(approvals.clone())),
                flux_runtime::ExecutionAuthorization::local(),
            )
            // flux-pin: first_app_session_persists_across_browser_messages
            .with_resource_limits(flux_runtime::ResourceLimits::autonomous());
            let app = match flux_app::App::try_with_execution_environment(
                module,
                Some(runtime.provider.clone()),
                runtime.model.clone(),
                environment,
                None,
                Arc::new(match flux_events::EventStore::in_memory() {
                    Ok(events) => events,
                    Err(error) => return internal_error(error),
                }),
                flux_app::HostPermissionRules::default(),
                vec!["bash".into(), "write".into(), "edit".into()],
            ) {
                Ok(app) => Arc::new(app),
                Err(error) => return invalid_source(error),
            };
            let mut ops = Vec::new();
            let mut destructive = false;
            let mut mutating = false;
            for flow in app
                .program()
                .journeys
                .iter()
                .map(|journey| &journey.flow)
                .chain(app.program().flows.iter())
            {
                let risk = flux_flow::runtime::plan_risk_with_composites(
                    flow,
                    app.registry(),
                    &app.program().ops,
                );
                for op in risk.ops {
                    if !ops.contains(&op) {
                        ops.push(op);
                    }
                }
                destructive |= risk.destructive;
                mutating |= risk.mutating;
            }
            if let Some(response) = forbidden_op(fixture, &ops) {
                return response;
            }
            let summary = if destructive {
                "destructive"
            } else if mutating {
                "mutating"
            } else {
                "low"
            }
            .to_string();
            (None, Some(app), summary, ops, destructive, mutating)
        }
    };
    let projection = flux_lang::editor::project_source(&request.source, None).ok();
    let id = random_token();
    let session = Arc::new(WorkbenchSession {
        owner,
        fixture,
        scratch,
        flow,
        app,
        approvals,
        run: tokio::sync::Mutex::new(RunState {
            status: RunStatus::Ready,
            result: None,
            error: None,
            tool_calls: Vec::new(),
        }),
        task: Mutex::new(None),
        events: Arc::new(Mutex::new(Vec::new())),
    });
    runtime
        .workbenches
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(id.clone(), session);
    Json(json!({
        "id": id,
        "fixture": fixture.id,
        "projection": projection,
        "risk": {
            "summary": risk_summary,
            "ops": risk_ops,
            "destructive": destructive,
            "mutating": mutating,
        },
        "lsp_url": format!("/api/workbench/sessions/{id}/lsp"),
    }))
    .into_response()
}

async fn run_session(
    State(state): State<DocsState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !same_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(session) = owned_session(&state, &headers, &id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    {
        let mut run = session.run.lock().await;
        if matches!(run.status, RunStatus::Running) {
            return StatusCode::CONFLICT.into_response();
        }
        run.status = RunStatus::Running;
        run.result = None;
        run.error = None;
        run.tool_calls.clear();
        session
            .events
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
    }
    let weak = Arc::downgrade(&session);
    let task = if let Some(flow) = &session.flow {
        let client = flow.client.clone();
        let ast = flow.ast.clone();
        let inputs = flow.inputs.clone();
        let events = session.events.clone();
        tokio::spawn(async move {
            let mut sink = WorkbenchSink { events };
            let result = client
                .execute_with_seeded_sink(&ast, inputs, &mut sink)
                .await;
            if let Some(session) = weak.upgrade() {
                let mut run = session.run.lock().await;
                match result {
                    Ok(result) => {
                        run.status = RunStatus::Succeeded;
                        run.result = Some(result.result);
                        run.tool_calls = result.tool_calls;
                    }
                    Err(error) => {
                        run.status = RunStatus::Failed;
                        run.error = Some(error.to_string());
                    }
                }
            }
        })
    } else if let Some(app) = &session.app {
        let app = app.clone();
        tokio::spawn(async move {
            let result = app.deliver("startup", json!({})).await;
            finish_app_delivery(weak, &app, result).await;
        })
    } else {
        return internal_error("workbench session has no executable program");
    };
    *session
        .task
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(task);
    (StatusCode::ACCEPTED, Json(json!({"status": "running"}))).into_response()
}

async fn deliver_app_input(
    State(state): State<DocsState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<AppInputRequest>,
) -> Response {
    if !same_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(session) = owned_session(&state, &headers, &id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(app) = session.app.clone() else {
        return StatusCode::CONFLICT.into_response();
    };
    {
        let mut run = session.run.lock().await;
        if matches!(run.status, RunStatus::Running) {
            return StatusCode::CONFLICT.into_response();
        }
        run.status = RunStatus::Running;
        run.result = None;
        run.error = None;
        run.tool_calls.clear();
        session
            .events
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clear();
    }
    let weak = Arc::downgrade(&session);
    let task_app = app.clone();
    let task = tokio::spawn(async move {
        let result = task_app
            .deliver("user_input", json!({"text": request.text}))
            .await;
        finish_app_delivery(weak, &task_app, result).await;
    });
    *session
        .task
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(task);
    (StatusCode::ACCEPTED, Json(json!({"status": "running"}))).into_response()
}

async fn finish_app_delivery(
    session: std::sync::Weak<WorkbenchSession>,
    app: &flux_app::App,
    result: flux_core::Result<Vec<flux_app::JourneyRun>>,
) {
    let Some(session) = session.upgrade() else {
        return;
    };
    let mut run = session.run.lock().await;
    match result {
        Ok(journeys) => {
            run.status = RunStatus::Succeeded;
            run.result = Some(
                app.bus()
                    .sent()
                    .into_iter()
                    .map(|message| message.message)
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
            run.tool_calls = journeys
                .into_iter()
                .map(|journey| format!("{} ({} steps)", journey.journey, journey.steps))
                .collect();
        }
        Err(error) => {
            run.status = RunStatus::Failed;
            run.error = Some(error.to_string());
        }
    }
}

async fn session_status(
    State(state): State<DocsState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Some(session) = owned_session(&state, &headers, &id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let run = session.run.lock().await;
    Json(json!({
        "fixture": session.fixture.id,
        "status": run.status,
        "result": run.result,
        "error": run.error,
        "tool_calls": run.tool_calls,
        "approvals": session.approvals.pending(),
        "events": session.events.lock().unwrap_or_else(|error| error.into_inner()).clone(),
        "files": session.fixture.allowed_files,
    }))
    .into_response()
}

async fn decide_approval(
    State(state): State<DocsState>,
    Path((id, approval)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<DecisionRequest>,
) -> Response {
    if !same_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(session) = owned_session(&state, &headers, &id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let choice = match request.choice.as_str() {
        "allow" => ApprovalChoice::Allow,
        "deny" => ApprovalChoice::Deny,
        _ => return StatusCode::UNPROCESSABLE_ENTITY.into_response(),
    };
    match session
        .approvals
        .decide(&approval, &request.fingerprint, choice)
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => (
            StatusCode::CONFLICT,
            Json(json!({"error": error.to_string()})),
        )
            .into_response(),
    }
}

async fn cancel_session(
    State(state): State<DocsState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !same_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(session) = owned_session(&state, &headers, &id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if let Some(task) = session
        .task
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take()
    {
        task.abort();
    }
    session.run.lock().await.status = RunStatus::Cancelled;
    StatusCode::NO_CONTENT.into_response()
}

async fn delete_session(
    State(state): State<DocsState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !same_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(owner) = authenticated_owner(&state, &headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(runtime) = &state.runtime else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let removed = {
        let mut sessions = runtime
            .workbenches
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if sessions
            .get(&id)
            .is_some_and(|session| session.owner == owner)
        {
            sessions.remove(&id)
        } else {
            None
        }
    };
    let Some(session) = removed else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if let Some(task) = session
        .task
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take()
    {
        task.abort();
    }
    StatusCode::NO_CONTENT.into_response()
}

fn internal_error(error: impl std::fmt::Display) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error": error.to_string()})),
    )
        .into_response()
}

fn invalid_source(error: impl std::fmt::Display) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({"error": error.to_string()})),
    )
        .into_response()
}

fn forbidden_op(fixture: &Fixture, ops: &[String]) -> Option<Response> {
    ops.iter()
        .find(|op| !fixture.allowed_ops.contains(&op.as_str()))
        .map(|op| forbidden_op_response(fixture, op))
}

fn forbidden_op_response(fixture: &Fixture, op: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "error": format!("operation {op:?} is outside fixture {}", fixture.id),
            "allowed_ops": fixture.allowed_ops,
        })),
    )
        .into_response()
}

async fn lsp_upgrade(
    State(state): State<DocsState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !same_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(session) = owned_session(&state, &headers, &id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let root = session.scratch.root().to_path_buf();
    upgrade
        .on_upgrade(move |socket| bridge_lsp(socket, root))
        .into_response()
}

async fn browser_lsp_upgrade(
    State(state): State<DocsState>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    if !same_origin(&headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(runtime) = &state.runtime else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(token) = cookie_token(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let root = runtime
        .browser_sessions
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(token)
        .map(|session| session.scratch.root().to_path_buf());
    let Some(root) = root else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    upgrade
        .on_upgrade(move |socket| bridge_lsp(socket, root))
        .into_response()
}

async fn bridge_lsp(socket: WebSocket, root: PathBuf) {
    let (client_to_bridge, lsp_input) = tokio::io::duplex(256 * 1024);
    let (lsp_output, bridge_to_client) = tokio::io::duplex(256 * 1024);
    tokio::spawn(flux_lsp::serve_io(
        lsp_input,
        lsp_output,
        flux_lsp::WorkspacePolicy::Fixed(Some(root)),
    ));

    let (mut ws_tx, mut ws_rx) = socket.split();
    let mut framed_input = client_to_bridge;
    let input = async move {
        while let Some(Ok(message)) = ws_rx.next().await {
            match message {
                Message::Text(body) => {
                    let frame = format!("Content-Length: {}\r\n\r\n{body}", body.len());
                    framed_input.write_all(frame.as_bytes()).await?;
                    framed_input.flush().await?;
                }
                Message::Close(_) => break,
                Message::Ping(_) | Message::Pong(_) | Message::Binary(_) => {}
            }
        }
        Ok::<(), std::io::Error>(())
    };
    let output = async move {
        let mut reader = BufReader::new(bridge_to_client);
        loop {
            let Some(length) = read_lsp_length(&mut reader).await? else {
                break;
            };
            if length > MAX_SOURCE_BODY_BYTES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "LSP message exceeds workbench limit",
                ));
            }
            let mut body = vec![0u8; length];
            reader.read_exact(&mut body).await?;
            let text = String::from_utf8(body)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
            if ws_tx.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
        Ok::<(), std::io::Error>(())
    };
    tokio::select! {
        _ = input => {}
        _ = output => {}
    }
}

async fn read_lsp_length<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
) -> std::io::Result<Option<usize>> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await? == 0 {
            return Ok(None);
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            return length.map(Some).ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "missing Content-Length")
            });
        }
        if let Some(value) = line.strip_prefix("Content-Length: ") {
            length =
                Some(value.parse().map_err(|error| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, error)
                })?);
        }
    }
}

async fn static_asset(State(state): State<DocsState>, request: Request<Body>) -> Response {
    let request_path = request.uri().path().trim_start_matches('/');
    if request_path == "api" || request_path.starts_with("api/") {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(asset_path) = resolve_asset_path(request_path, &state.assets) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let asset = &state.assets[asset_path];
    let mut response = Response::new(Body::from(asset.bytes.clone()));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static(asset.content_type),
    );
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static(if asset.immutable {
            "public, max-age=31536000, immutable"
        } else {
            "no-cache"
        }),
    );
    response
}

fn resolve_asset_path<'a>(
    request_path: &str,
    assets: &'a HashMap<String, Asset>,
) -> Option<&'a str> {
    let relative = if request_path == "console" || request_path == "console/" {
        "console.html"
    } else if request_path.starts_with("console/") {
        request_path
    } else if request_path == "flux" || request_path == "flux/" {
        "index.html"
    } else {
        request_path.strip_prefix("flux/")?
    };
    if assets.contains_key(relative) {
        return assets.get_key_value(relative).map(|(key, _)| key.as_str());
    }
    let html = format!("{relative}.html");
    if assets.contains_key(&html) {
        return assets.get_key_value(&html).map(|(key, _)| key.as_str());
    }
    let index = format!("{}/index.html", relative.trim_end_matches('/'));
    assets.get_key_value(&index).map(|(key, _)| key.as_str())
}

fn load_assets() -> anyhow::Result<HashMap<String, Asset>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(SITE_ZIP))?;
    let mut assets = HashMap::with_capacity(archive.len());
    for index in 0..archive.len() {
        let mut file = archive.by_index(index)?;
        if file.is_dir() {
            continue;
        }
        let path = file
            .enclosed_name()
            .ok_or_else(|| anyhow::anyhow!("embedded docs contain an unsafe archive path"))?
            .to_string_lossy()
            .trim_start_matches("./")
            .to_string();
        let mut bytes = Vec::with_capacity(file.size() as usize);
        file.read_to_end(&mut bytes)?;
        let immutable = path.starts_with("assets/") || path.contains("/assets/");
        assets.insert(
            path.clone(),
            Asset {
                bytes: bytes.into(),
                content_type: content_type(&path),
                immutable,
            },
        );
    }
    if !assets.contains_key("index.html") || !assets.contains_key("console.html") {
        anyhow::bail!("embedded docs bundle is missing its site or console entry point");
    }
    Ok(assets)
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or_default() {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "xml" => "application/xml; charset=utf-8",
        "txt" => "text/plain; charset=utf-8",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::Arc;
    use std::time::Duration;

    use axum::body::{to_bytes, Body};
    use axum::http::{header, HeaderMap, Request, StatusCode};
    use flux_provider::{NullProvider, Provider, StaticProvider};
    use futures::{SinkExt as _, StreamExt as _};
    use serde_json::Value;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
    use tower::ServiceExt;

    use super::{docs_app, docs_app_for_bind};

    async fn response(method: &str, path: &str, body: &str) -> (StatusCode, Vec<u8>) {
        let response = docs_app("9.8.7")
            .unwrap()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec();
        (status, body)
    }

    #[tokio::test]
    async fn version_and_embedded_entry_points_ship_together() {
        let (status, body) = response("GET", "/version", "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap()["version"],
            "9.8.7"
        );

        for path in ["/flux/", "/console/"] {
            let (status, body) = response("GET", path, "").await;
            assert_eq!(status, StatusCode::OK, "{path}");
            assert!(
                String::from_utf8_lossy(&body).contains("flux"),
                "{path} must be served from the embedded site"
            );
        }
    }

    #[tokio::test]
    async fn projection_uses_the_real_editor_contract_and_parse_errors_are_bounded() {
        let source = r#"flow demo -> String
  first = demo_step(label: "one")
  return first
"#;
        let request = serde_json::json!({"source": source}).to_string();
        let (status, body) = response("POST", "/api/playground/project", &request).await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["graph"]["schema_version"], 1);
        assert_eq!(json["graph"]["body"][0]["kind"], "call");
        assert_eq!(json["graph"]["body"][1]["kind"], "return");

        let bad = serde_json::json!({"source": "flow broken\n  return ("}).to_string();
        let (status, body) = response("POST", "/api/playground/project", &bad).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert!(json["error"]
            .as_str()
            .is_some_and(|error| !error.is_empty()));
    }

    #[tokio::test]
    async fn unknown_api_paths_never_fall_through_to_the_site() {
        for path in ["/api", "/api/", "/api/not-a-route"] {
            let (status, body) = response("GET", path, "").await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
            assert!(!String::from_utf8_lossy(&body).contains("<!doctype html>"));
        }
    }

    #[tokio::test]
    async fn non_loopback_docs_never_construct_or_mount_a_runtime() {
        let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 8788);
        let (app, launch) =
            docs_app_for_bind("9.8.7", bind, Arc::new(NullProvider), "null".into()).unwrap();
        assert!(launch.is_none());
        for path in [
            "/api/workbench/exchange",
            "/api/workbench/bootstrap",
            "/api/workbench/sessions",
            "/api/workbench/lsp",
        ] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }
    }

    async fn local_runtime() -> (axum::Router, String) {
        local_runtime_with(Arc::new(NullProvider)).await
    }

    async fn local_runtime_with(provider: Arc<dyn Provider>) -> (axum::Router, String) {
        let bind = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8788);
        let (app, launch) =
            docs_app_for_bind("9.8.7", bind, provider, "test-model".into()).unwrap();
        (app, launch.unwrap())
    }

    async fn call(
        app: &axum::Router,
        method: &str,
        path: &str,
        body: Value,
        cookie: Option<&str>,
    ) -> (StatusCode, HeaderMap, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, "127.0.0.1:8788")
            .header(header::ORIGIN, "http://127.0.0.1:8788")
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(cookie) = cookie {
            request = request.header(header::COOKIE, cookie);
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::from(body.to_string())).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
        };
        (status, headers, body)
    }

    async fn authenticate(app: &axum::Router, launch: &str) -> String {
        let (status, headers, _) = call(
            app,
            "POST",
            "/api/workbench/exchange",
            serde_json::json!({"secret": launch}),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        headers
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string()
    }

    #[tokio::test]
    async fn undeclared_examples_cannot_be_executed() {
        let (app, launch) = local_runtime().await;
        let cookie = authenticate(&app, &launch).await;
        let (status, _, body) = call(
            &app,
            "POST",
            "/api/workbench/sessions",
            serde_json::json!({
                "fixture": "route-ticket",
                "source": "flow route-ticket\n  return \"no\"\n"
            }),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

        let (status, _, body) = call(
            &app,
            "POST",
            "/api/workbench/sessions",
            serde_json::json!({
                "fixture": "wait-for-artifact",
                "source": "flow escape\n  return bash(\"pwd\")\n",
                "files": {"target/release/flux": "fixture"}
            }),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

        let (status, _, body) = call(
            &app,
            "POST",
            "/api/workbench/sessions",
            serde_json::json!({
                "fixture": "first-app-a",
                "source": r#"agent guide
  tools [search]
  datasources [handbook]

channel cli

datasource handbook
  kind "markdown"
  path "./docs"

trigger welcome
  on "startup"
  run show-welcome

journey show-welcome
  flow
    send(channel: "cli", message: "ready")
    return ""

flow hidden -> String
  return ai.reason(ask: "this flow must not be smuggled into the app")
"#,
                "files": {"docs/product.md": "fixture"}
            }),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    }

    #[tokio::test]
    async fn run_is_scratch_scoped_and_approval_bound() {
        let (app, launch) = local_runtime().await;
        let cookie = authenticate(&app, &launch).await;
        let source = "flow wait-for-artifact\n  found = path_exists(\"target/release/flux\")\n  return found\n";
        let (status, _, prepared) = call(
            &app,
            "POST",
            "/api/workbench/sessions",
            serde_json::json!({
                "fixture": "wait-for-artifact",
                "source": source,
                "files": {"target/release/flux": "scratch-only"}
            }),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{prepared}");
        let id = prepared["id"].as_str().unwrap();
        let (status, _, _) = call(
            &app,
            "POST",
            &format!("/api/workbench/sessions/{id}/run"),
            Value::Null,
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);

        let pending = loop {
            let (_, _, state) = call(
                &app,
                "GET",
                &format!("/api/workbench/sessions/{id}"),
                Value::Null,
                Some(&cookie),
            )
            .await;
            if let Some(pending) = state["approvals"]
                .as_array()
                .and_then(|items| items.first())
            {
                break pending.clone();
            }
            tokio::task::yield_now().await;
        };
        let approval = pending["id"].as_str().unwrap();
        let fingerprint = pending["fingerprint"].as_str().unwrap();
        let (status, _, _) = call(
            &app,
            "POST",
            &format!("/api/workbench/sessions/{id}/approvals/{approval}"),
            serde_json::json!({"fingerprint": "different-effect", "choice": "allow"}),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        let (status, _, _) = call(
            &app,
            "POST",
            &format!("/api/workbench/sessions/{id}/approvals/{approval}"),
            serde_json::json!({"fingerprint": fingerprint, "choice": "allow"}),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        for _ in 0..100 {
            let (_, _, state) = call(
                &app,
                "GET",
                &format!("/api/workbench/sessions/{id}"),
                Value::Null,
                Some(&cookie),
            )
            .await;
            if state["status"] == "succeeded" {
                assert!(state["result"]
                    .as_str()
                    .is_some_and(|value| value.contains("true")));
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("scratch run did not complete after approval");
    }

    async fn wait_for_terminal(app: &axum::Router, cookie: &str, id: &str) -> Value {
        for _ in 0..100 {
            let (_, _, state) = call(
                app,
                "GET",
                &format!("/api/workbench/sessions/{id}"),
                Value::Null,
                Some(cookie),
            )
            .await;
            if state["status"] != "running" {
                return state;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("workbench session did not reach a terminal state");
    }

    #[tokio::test]
    async fn first_app_session_persists_across_browser_messages() {
        let (app, launch) =
            local_runtime_with(Arc::new(StaticProvider::new("Use the synchronized copy."))).await;
        let cookie = authenticate(&app, &launch).await;
        let source = r#"permissions
  allow [search, "ai.reason", send]
  deny [write, edit, bash]

channel cli

datasource handbook
  kind "markdown"
  path "./docs"

trigger welcome
  on "startup"
  run show-welcome

trigger questions
  on "user_input"
  run answer-question

journey show-welcome
  flow
    send(channel: "cli", message: "Northstar ready")
    return ""

journey answer-question
  flow
    hits = search(query: "{text}", source: "handbook")
    answer = ai.reason(ask: "Question: {text}\nUse: {hits}")
    send(channel: "cli", message: "{answer}")
    return ""
"#;
        let (status, _, prepared) = call(
            &app,
            "POST",
            "/api/workbench/sessions",
            serde_json::json!({
                "fixture": "first-app-b",
                "source": source,
                "files": {
                    "docs/product.md": "Offline edits synchronize after reconnecting.",
                    "docs/policies.md": "Support is open on weekdays."
                }
            }),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{prepared}");
        let id = prepared["id"].as_str().unwrap();
        let (status, _, _) = call(
            &app,
            "POST",
            &format!("/api/workbench/sessions/{id}/run"),
            Value::Null,
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let startup = wait_for_terminal(&app, &cookie, id).await;
        assert_eq!(startup["status"], "succeeded", "{startup}");
        assert!(startup["result"]
            .as_str()
            .unwrap()
            .contains("Northstar ready"));

        let (status, _, _) = call(
            &app,
            "POST",
            &format!("/api/workbench/sessions/{id}/input"),
            serde_json::json!({"text": "What happens offline?"}),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let answer = wait_for_terminal(&app, &cookie, id).await;
        assert_eq!(answer["status"], "succeeded", "{answer}");
        let messages = answer["result"].as_str().unwrap();
        assert!(
            messages.contains("Northstar ready"),
            "startup state was lost: {messages}"
        );
        assert!(
            messages.contains("Use the synchronized copy."),
            "{messages}"
        );
    }

    #[tokio::test]
    async fn first_app_page_declares_two_valid_runnable_programs() {
        let page = include_str!("../../../website/docs/tutorial/first-app.md");
        let (app, launch) =
            local_runtime_with(Arc::new(StaticProvider::new("handbook answer"))).await;
        let cookie = authenticate(&app, &launch).await;
        for fixture in ["first-app-a", "first-app-b"] {
            let marker = format!("```flux runnable=\"{fixture}\"");
            let after = page
                .split_once(&marker)
                .unwrap_or_else(|| panic!("missing {fixture} runnable fence"))
                .1;
            let source = after
                .split_once('\n')
                .and_then(|(_, body)| body.split_once("\n```").map(|(source, _)| source))
                .unwrap();
            let (status, _, body) = call(
                &app,
                "POST",
                "/api/workbench/sessions",
                serde_json::json!({
                    "fixture": fixture,
                    "source": source,
                    "files": {
                        "docs/product.md": "Offline edits synchronize after reconnecting.",
                        "docs/policies.md": "Support is Monday-Friday, 09:00-17:00 CET."
                    }
                }),
                Some(&cookie),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{fixture}: {body}");
        }
    }

    #[tokio::test]
    async fn authenticated_websocket_speaks_standard_lsp_json_rpc() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (app, launch) =
            docs_app_for_bind("9.8.7", addr, Arc::new(NullProvider), "null".into()).unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let origin = format!("http://{addr}");
        let exchange = reqwest::Client::new()
            .post(format!("{origin}/api/workbench/exchange"))
            .header(header::ORIGIN, &origin)
            .json(&serde_json::json!({"secret": launch.unwrap()}))
            .send()
            .await
            .unwrap();
        assert_eq!(exchange.status(), StatusCode::OK);
        let cookie = exchange
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();

        let mut request = format!("ws://{addr}/api/workbench/lsp")
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert(header::ORIGIN, origin.parse().unwrap());
        request
            .headers_mut()
            .insert(header::COOKIE, cookie.parse().unwrap());
        let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {
                        "capabilities": {},
                        "rootUri": "file:///hostile-browser-root"
                    }
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        let response = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let message = socket.next().await.unwrap().unwrap();
                if let tokio_tungstenite::tungstenite::Message::Text(text) = message {
                    let value: Value = serde_json::from_str(&text).unwrap();
                    if value["id"] == 1 {
                        break value;
                    }
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(response["result"]["serverInfo"]["name"], "flux-lsp");
        server.abort();
    }
}
