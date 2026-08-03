//! Native client for Exchange's authenticated effective catalogue and one-shot invoke contract.
//!
//! The client is bound once to an operator-supplied Exchange origin and one Service Account token.
//! Model input is only ever the selected operation's declared JSON body: tenant, credential,
//! endpoint, grant, runtime and connection selection have no input field on this side of the wire.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use flux_core::{Error, Result};
use flux_runtime::{
    CatalogRefresher, LiveToolCatalog, Tool, ToolContext, ToolRegistry, ToolResult,
};
use flux_secret::Redactor;
use flux_spec::{AccessKind, Effect, Idempotency, Risk, ToolSpec};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use serde::Deserialize;
use serde_json::{json, Value};
use url::{Host, Url};

use crate::egress;

const CATALOGUE_PATH: &str = "api/catalogue/effective";
const MAX_CATALOGUE_BYTES: usize = 2 * 1024 * 1024;
const MAX_INVOKE_BYTES: usize = 512 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_OPERATIONS: usize = 2_000;

/// One compiled-in binding to one Exchange Service Account.
pub struct ExchangeClient {
    shared: reqwest::Client,
    base: Url,
    pinned: Vec<std::net::SocketAddr>,
    authorization: HeaderValue,
    redactor: Redactor,
    state: Mutex<Published>,
}

#[derive(Default)]
struct Published {
    generation: Option<String>,
    names: HashSet<String>,
}

/// Turn-boundary publication adapter for an [`ExchangeClient`].
pub struct ExchangeCatalogRefresher {
    client: Arc<ExchangeClient>,
}

impl ExchangeCatalogRefresher {
    pub fn new(client: Arc<ExchangeClient>) -> Self {
        Self { client }
    }
}

#[derive(Debug, Deserialize)]
struct EffectiveCatalogue {
    generation: String,
    operations: Vec<Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct EffectiveOperation {
    id: String,
    description: String,
    input_schema: Value,
    effects: Vec<Effect>,
    risk: Risk,
    idempotency: Idempotency,
    admitted: bool,
    connection: Option<String>,
}

impl ExchangeClient {
    /// Bind to one operator-selected origin and one canonical Service Account bearer.
    pub fn new(base: &str, token: &str, redactor: Redactor) -> Result<Self> {
        let token = token.trim();
        let mut parsed = Url::parse(base.trim())
            .map_err(|error| Error::Other(format!("invalid Exchange URL: {error}")))?;
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return Err(Error::Other(
                "Exchange URL must be an absolute http(s) origin".into(),
            ));
        }
        if !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || !matches!(parsed.path(), "" | "/")
        {
            return Err(Error::Other(
                "Exchange URL must contain only scheme, host and optional port".into(),
            ));
        }
        if parsed.scheme() == "http" && !is_loopback_origin(&parsed) {
            return Err(Error::Other(
                "Exchange URL must use HTTPS unless its host is loopback".into(),
            ));
        }
        parsed.set_path("/");
        let host = parsed
            .host_str()
            .expect("an absolute URL checked above has a host")
            .to_owned();
        let private_net = flux_system::net::PrivateNetAllow::from_hosts([host]);
        let (base, pinned) =
            flux_system::net::guard_url_scoped_pinned(parsed.as_str(), &private_net)?;
        if pinned.is_empty() {
            return Err(Error::Http(
                "Exchange URL resolved to no vetted address; refusing an unpinned client".into(),
            ));
        }
        if base.scheme() == "http" && pinned.iter().any(|address| !address.ip().is_loopback()) {
            return Err(Error::Other(
                "cleartext Exchange URL resolved beyond loopback; refusing to send its bearer"
                    .into(),
            ));
        }
        redactor
            .try_add_secret(token.to_owned())
            .map_err(|why| Error::Other(format!("Exchange Service Account token: {why}")))?;
        let mut authorization =
            HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| {
                Error::Other(
                    "Exchange Service Account token is not valid HTTP header material".into(),
                )
            })?;
        authorization.set_sensitive(true);
        Ok(Self {
            shared: egress::redirect_disabled_client(),
            base,
            pinned,
            authorization,
            redactor,
            state: Mutex::new(Published::default()),
        })
    }

    /// Fetch and install the current generation into an assembly-time registry.
    pub async fn refresh_registry(self: &Arc<Self>, registry: &mut ToolRegistry) -> Result<String> {
        let catalogue = self.fetch_catalogue().await?;
        let generation = catalogue.generation.clone();
        let tools = self.project(catalogue)?;
        let source = format!("exchange effective catalogue {generation}");
        registry.try_register_all_from(source, tools.iter().cloned())?;
        let names = tools.into_iter().map(|tool| tool.spec().name).collect();
        *self
            .state
            .lock()
            .expect("Exchange catalogue state is not poisoned") = Published {
            generation: Some(generation.clone()),
            names,
        };
        Ok(generation)
    }

    async fn fetch_catalogue(&self) -> Result<EffectiveCatalogue> {
        let url = self
            .base
            .join(CATALOGUE_PATH)
            .map_err(|error| Error::Other(format!("Exchange catalogue URL: {error}")))?;
        let client = egress::pinned_client(&self.shared, &url, &self.pinned, "exchange.catalogue")?;
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, self.authorization.clone());
        let response = client
            .get(url)
            .headers(headers)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|error| Error::Http(format!("Exchange unavailable: {error}")))?;
        if response.status().is_redirection() {
            return Err(Error::Http(
                "Exchange refused: redirects are not part of the bound Service Account origin"
                    .into(),
            ));
        }
        let status = response.status();
        let body =
            egress::read_body_capped(response, MAX_CATALOGUE_BYTES, "exchange.catalogue").await?;
        if body.truncated {
            return Err(Error::Http(format!(
                "Exchange catalogue exceeds the {MAX_CATALOGUE_BYTES}-byte client limit"
            )));
        }
        if !status.is_success() {
            let detail = self.redactor.redact(&String::from_utf8_lossy(&body.bytes));
            let class = if status == reqwest::StatusCode::UNAUTHORIZED {
                "authentication failed"
            } else {
                "catalogue unavailable"
            };
            return Err(Error::Http(format!(
                "Exchange {class} (HTTP {}): {}",
                status.as_u16(),
                detail
            )));
        }
        let catalogue: EffectiveCatalogue =
            serde_json::from_slice(&body.bytes).map_err(|error| {
                Error::Other(format!("invalid Exchange effective catalogue: {error}"))
            })?;
        if !catalogue.generation.starts_with("sha256:") || catalogue.generation.len() != 71 {
            return Err(Error::Other(
                "Exchange effective catalogue has no valid sha256 generation identity".into(),
            ));
        }
        if catalogue.operations.len() > MAX_OPERATIONS {
            return Err(Error::Other(format!(
                "Exchange effective catalogue has {} operations; client limit is {MAX_OPERATIONS}",
                catalogue.operations.len()
            )));
        }
        Ok(catalogue)
    }

    fn project(self: &Arc<Self>, catalogue: EffectiveCatalogue) -> Result<Vec<Arc<dyn Tool>>> {
        let mut decoded = Vec::with_capacity(catalogue.operations.len());
        for raw in catalogue.operations {
            let object = raw.as_object().ok_or_else(|| {
                Error::Other("Exchange effective operation is not an object".into())
            })?;
            for forbidden in [
                "tenant",
                "credential",
                "endpoint",
                "grant",
                "runtime",
                "instance",
            ] {
                if object.contains_key(forbidden) {
                    return Err(Error::Other(format!(
                        "Exchange effective operation contains forbidden authority field `{forbidden}`"
                    )));
                }
            }
            let operation: EffectiveOperation = serde_json::from_value(raw).map_err(|error| {
                Error::Other(format!("invalid Exchange effective operation: {error}"))
            })?;
            if !operation.admitted {
                return Err(Error::Other(format!(
                    "Exchange advertised `{}` without positively admitting it",
                    operation.id
                )));
            }
            if !operation.input_schema.is_object() {
                return Err(Error::Other(format!(
                    "Exchange operation `{}` has a non-object input schema",
                    operation.id
                )));
            }
            decoded.push(operation);
        }

        let mut counts = BTreeMap::<String, usize>::new();
        for operation in &decoded {
            *counts.entry(operation.id.clone()).or_default() += 1;
        }
        let mut names = HashSet::new();
        let mut tools = Vec::with_capacity(decoded.len());
        for operation in decoded {
            let name = if counts.get(&operation.id).copied().unwrap_or(0) == 1 {
                operation.id.clone()
            } else {
                let label = operation.connection.as_deref().ok_or_else(|| {
                    Error::Other(format!(
                        "Exchange returned ambiguous unlabelled bindings for `{}`",
                        operation.id
                    ))
                })?;
                format!("{}__{}", operation.id, tool_label(label))
            };
            if !names.insert(name.clone()) {
                return Err(Error::Other(format!(
                    "Exchange bindings collide on projected operation name `{name}`"
                )));
            }
            let mut description = operation.description.clone();
            if counts.get(&operation.id).copied().unwrap_or(0) > 1 {
                description.push_str(&format!(
                    " (Exchange connection `{}`.)",
                    operation.connection.as_deref().unwrap_or_default()
                ));
            }
            let subject = self.invocation_url(&operation.id, operation.connection.as_deref())?;
            let spec = ToolSpec {
                name,
                description,
                input_schema: operation.input_schema.clone(),
                output_schema: None,
                effects: operation.effects.clone(),
                risk: operation.risk,
                idempotency: operation.idempotency,
                access: vec![AccessKind::Network],
                group: None,
            };
            tools.push(Arc::new(ExchangeOperation {
                client: self.clone(),
                operation: operation.id,
                connection: operation.connection,
                subject: subject.to_string(),
                spec,
            }) as Arc<dyn Tool>);
        }
        Ok(tools)
    }

    fn withdraw(&self, catalog: &LiveToolCatalog) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .expect("Exchange catalogue state is not poisoned");
        let names = state.names.clone();
        if names.is_empty() {
            state.generation = None;
            return Ok(());
        }
        catalog.try_update(|registry| {
            for name in &names {
                registry.remove(name);
            }
            Ok(())
        })?;
        state.names.clear();
        state.generation = None;
        Ok(())
    }

    async fn invoke(
        &self,
        operation: &str,
        connection: Option<&str>,
        params: Value,
    ) -> Result<ToolResult> {
        let url = self.invocation_url(operation, connection)?;
        let client = egress::pinned_client(&self.shared, &url, &self.pinned, "exchange.invoke")?;
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, self.authorization.clone());
        let response = client
            .post(url)
            .headers(headers)
            .json(&params)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|error| Error::Http(format!("Exchange unavailable: {error}")))?;
        if response.status().is_redirection() {
            return Err(Error::Http(
                "Exchange invocation redirected away from its bound origin".into(),
            ));
        }
        let status = response.status();
        let body = egress::read_body_capped(response, MAX_INVOKE_BYTES, "exchange.invoke").await?;
        if body.truncated {
            return Err(Error::Http(format!(
                "Exchange invocation response exceeds the {MAX_INVOKE_BYTES}-byte client limit"
            )));
        }
        let value: Value = serde_json::from_slice(&body.bytes).unwrap_or_else(|_| {
            Value::String(self.redactor.redact(&String::from_utf8_lossy(&body.bytes)))
        });
        if !status.is_success() {
            let value = redact_value(&self.redactor, value);
            let kind = canonical_failure_kind(status, &value);
            return Ok(ToolResult::error(
                json!({
                    "source": "exchange",
                    "kind": kind,
                    "status": status.as_u16(),
                    "refusal": value.get("refusal"),
                    "code": value.get("code"),
                    "sent": value.get("sent"),
                    "retryable": value.get("retryable"),
                    "message": value.get("message").or_else(|| value.get("error")),
                    "body": value,
                })
                .to_string(),
            ));
        }
        let content = value
            .get("content")
            .map(canonical_content)
            .unwrap_or_else(|| value.to_string());
        let view = value.get("view").and_then(Value::as_str).map(str::to_owned);
        Ok(ToolResult {
            content,
            view,
            is_error: value
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }

    fn invocation_url(&self, operation: &str, connection: Option<&str>) -> Result<Url> {
        let mut url = self.base.clone();
        {
            let mut segments = url.path_segments_mut().map_err(|_| {
                Error::Other("Exchange base URL cannot carry API path segments".into())
            })?;
            segments.extend(["api", "operations", operation, "invoke"]);
        }
        if let Some(connection) = connection {
            url.query_pairs_mut().append_pair("connection", connection);
        }
        Ok(url)
    }
}

#[async_trait]
impl CatalogRefresher for ExchangeCatalogRefresher {
    async fn refresh(&self, catalog: &LiveToolCatalog) -> Result<()> {
        let catalogue = match self.client.fetch_catalogue().await {
            Ok(catalogue) => catalogue,
            Err(error) => {
                self.client.withdraw(catalog)?;
                return Err(error);
            }
        };
        {
            let state = self
                .client
                .state
                .lock()
                .expect("Exchange catalogue state is not poisoned");
            if state.generation.as_deref() == Some(&catalogue.generation) {
                return Ok(());
            }
        }
        let generation = catalogue.generation.clone();
        let tools = match self.client.project(catalogue) {
            Ok(tools) => tools,
            Err(error) => {
                self.client.withdraw(catalog)?;
                return Err(error);
            }
        };
        let mut state = self
            .client
            .state
            .lock()
            .expect("Exchange catalogue state is not poisoned");
        let previous = state.names.clone();
        let source = format!("exchange effective catalogue {generation}");
        if let Err(error) = catalog.try_update(|registry| {
            for name in &previous {
                registry.remove(name);
            }
            registry.try_register_all_from(source, tools.iter().cloned())
        }) {
            drop(state);
            self.client.withdraw(catalog)?;
            return Err(error);
        }
        state.names = tools.into_iter().map(|tool| tool.spec().name).collect();
        state.generation = Some(generation);
        Ok(())
    }
}

struct ExchangeOperation {
    client: Arc<ExchangeClient>,
    operation: String,
    connection: Option<String>,
    subject: String,
    spec: ToolSpec,
}

#[async_trait]
impl Tool for ExchangeOperation {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    fn permission_subjects(&self, _params: &Value) -> Vec<String> {
        vec![self.subject.clone()]
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        self.client
            .invoke(&self.operation, self.connection.as_deref(), params)
            .await
    }
}

fn tool_label(label: &str) -> String {
    label
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn is_loopback_origin(url: &Url) -> bool {
    match url.host() {
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        Some(Host::Domain(domain)) => {
            domain.eq_ignore_ascii_case("localhost")
                || domain
                    .to_ascii_lowercase()
                    .strip_suffix(".localhost")
                    .is_some_and(|prefix| !prefix.is_empty())
        }
        None => false,
    }
}

fn canonical_failure_kind(status: reqwest::StatusCode, body: &Value) -> String {
    if let Some(refusal) = body.get("refusal").and_then(Value::as_str) {
        return refusal.to_owned();
    }
    if let Some(code) = body.get("code").and_then(Value::as_str) {
        return code.to_owned();
    }
    match status.as_u16() {
        401 => "authentication",
        403 => "grant_refusal",
        409 => "runtime_refusal",
        502..=504 => "exchange_unavailable",
        _ => "exchange_refusal",
    }
    .to_owned()
}

fn canonical_content(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn redact_value(redactor: &Redactor, value: Value) -> Value {
    match value {
        Value::String(text) => Value::String(redactor.redact(&text)),
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(|value| redact_value(redactor, value))
                .collect(),
        ),
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, redact_value(redactor, value)))
                .collect(),
        ),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};

    use flux_runtime::{AllowApprover, Executor, PermissionManager, ToolContext};
    use flux_system::{System, Workspace};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    static NEXT_WORKSPACE: AtomicU64 = AtomicU64::new(0);

    struct CoreTool;

    #[async_trait]
    impl Tool for CoreTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "core.echo".into(),
                description: "Core capability retained when Exchange is unavailable".into(),
                input_schema: json!({"type": "object"}),
                output_schema: None,
                effects: vec![Effect::Read],
                risk: Risk::Low,
                idempotency: Idempotency::Idempotent,
                access: vec![],
                group: None,
            }
        }

        async fn execute(&self, _ctx: &ToolContext, _params: Value) -> Result<ToolResult> {
            Ok(ToolResult::ok("core-ok"))
        }
    }

    fn test_context(redactor: Redactor) -> ToolContext {
        let path = std::env::temp_dir().join(format!(
            "flux-exchange-test-{}-{}",
            std::process::id(),
            NEXT_WORKSPACE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        ToolContext::new(Arc::new(System::new(Workspace::new(path).unwrap())))
            .with_redactor(redactor)
    }

    async fn read_request(socket: &mut tokio::net::TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let read = socket.read(&mut buffer).await.unwrap();
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..read]);
            let Some(headers_end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") else {
                continue;
            };
            let headers = String::from_utf8_lossy(&bytes[..headers_end]);
            let length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if bytes.len() >= headers_end + 4 + length {
                break;
            }
        }
        String::from_utf8(bytes).unwrap()
    }

    fn effective_catalogue() -> String {
        let operations = [
            ("vendor.ok", 200),
            ("vendor.auth", 401),
            ("vendor.grant", 403),
            ("vendor.runtime", 409),
            ("vendor.transport", 502),
            ("vendor.disconnected", 409),
            ("vendor.ambiguous", 409),
            ("vendor.down", 503),
        ]
        .into_iter()
        .map(|(id, _)| {
            json!({
                "id": id,
                "description": format!("Invoke {id}"),
                "input_schema": {"type": "object", "properties": {"message": {"type": "string"}}},
                "effects": ["read", "network"],
                "risk": "low",
                "idempotency": "idempotent",
                "admitted": true,
                "connection": "operator-selected"
            })
        })
        .collect::<Vec<_>>();
        json!({
            "generation": format!("sha256:{}", "0".repeat(64)),
            "operations": operations
        })
        .to_string()
    }

    async fn exchange_server(
        mode: Arc<AtomicU8>,
        requests: Arc<Mutex<Vec<String>>>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let request = read_request(&mut socket).await;
                requests.lock().unwrap().push(request.clone());
                let path = request.lines().next().unwrap_or_default();
                let (status, body) = if path.contains("/api/catalogue/effective") {
                    if mode.load(Ordering::SeqCst) == 0 {
                        ("200 OK", effective_catalogue())
                    } else {
                        (
                            "503 Service Unavailable",
                            json!({"error": "offline"}).to_string(),
                        )
                    }
                } else if path.contains("/vendor.auth/") {
                    (
                        "401 Unauthorized",
                        json!({
                            "error": "bad service account",
                            "echo": "flux-service-account-token-123"
                        })
                        .to_string(),
                    )
                } else if path.contains("/vendor.grant/") {
                    (
                        "403 Forbidden",
                        json!({
                            "refusal": "not_granted",
                            "operation": "vendor.grant",
                            "sent": "no",
                            "retryable": false,
                            "message": "grant denied"
                        })
                        .to_string(),
                    )
                } else if path.contains("/vendor.runtime/") {
                    (
                        "409 Conflict",
                        json!({
                            "refusal": "runtime_refused",
                            "operation": null,
                            "sent": "no",
                            "retryable": false,
                            "message": "connector runtime refused"
                        })
                        .to_string(),
                    )
                } else if path.contains("/vendor.transport/") {
                    (
                        "502 Bad Gateway",
                        json!({
                            "refusal": "transport",
                            "operation": "vendor.transport",
                            "sent": "maybe",
                            "retryable": true,
                            "message": "vendor transport failed"
                        })
                        .to_string(),
                    )
                } else if path.contains("/vendor.disconnected/") {
                    (
                        "409 Conflict",
                        json!({
                            "code": "disconnected",
                            "connector": "vendor",
                            "error": "connect it before invoking"
                        })
                        .to_string(),
                    )
                } else if path.contains("/vendor.ambiguous/") {
                    (
                        "409 Conflict",
                        json!({
                            "code": "ambiguous_connection",
                            "connector": "vendor",
                            "error": "choose a connection label"
                        })
                        .to_string(),
                    )
                } else if path.contains("/vendor.down/") {
                    (
                        "503 Service Unavailable",
                        json!({"error": "offline"}).to_string(),
                    )
                } else {
                    ("200 OK", json!({"content": "vendor-ok"}).to_string())
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        (base, task)
    }

    #[tokio::test]
    async fn bound_catalogue_dispatches_and_preserves_exchange_refusal_identity() {
        const SERVICE_TOKEN: &str = "flux-service-account-token-123";

        let mode = Arc::new(AtomicU8::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (base, server) = exchange_server(mode.clone(), requests.clone()).await;
        let redactor = Redactor::new();
        let client = Arc::new(ExchangeClient::new(&base, SERVICE_TOKEN, redactor.clone()).unwrap());

        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(CoreTool));
        client.refresh_registry(&mut registry).await.unwrap();
        assert_eq!(
            registry.names(),
            vec![
                "core.echo",
                "vendor.ambiguous",
                "vendor.auth",
                "vendor.disconnected",
                "vendor.down",
                "vendor.grant",
                "vendor.ok",
                "vendor.runtime",
                "vendor.transport"
            ]
        );

        let context = test_context(redactor);
        let executor = Executor::new(
            registry,
            PermissionManager::from_rules(
                &[
                    "core.echo".into(),
                    "vendor.ambiguous".into(),
                    "vendor.auth".into(),
                    "vendor.disconnected".into(),
                    "vendor.down".into(),
                    "vendor.grant".into(),
                    "vendor.ok".into(),
                    "vendor.runtime".into(),
                    "vendor.transport".into(),
                ],
                &[],
            ),
            Arc::new(AllowApprover),
            context.clone(),
        );
        let result = executor
            .dispatch("vendor.ok", json!({"message": "hello"}))
            .await;
        assert_eq!(result.content, "vendor-ok");
        assert!(!result.content.contains(SERVICE_TOKEN));
        let evidence = serde_json::to_string(&*context.evidence.lock().unwrap()).unwrap();
        assert!(!evidence.contains(SERVICE_TOKEN));

        let captured = requests.lock().unwrap().join("\n");
        assert!(captured.contains(&format!("Bearer {SERVICE_TOKEN}")));
        assert!(captured.contains("connection=operator-selected"));
        assert!(captured.contains(r#"{"message":"hello"}"#));
        for forbidden in [
            "tenant",
            "credential",
            "endpoint",
            "grant",
            "runtime",
            "instance",
        ] {
            assert!(!captured.contains(&format!("\"{forbidden}\"")));
        }

        for (operation, kind) in [
            ("vendor.auth", "authentication"),
            ("vendor.grant", "not_granted"),
            ("vendor.runtime", "runtime_refused"),
            ("vendor.transport", "transport"),
            ("vendor.disconnected", "disconnected"),
            ("vendor.ambiguous", "ambiguous_connection"),
            ("vendor.down", "exchange_unavailable"),
        ] {
            let failure = executor.dispatch(operation, json!({})).await;
            assert!(failure.is_error);
            let payload: Value = serde_json::from_str(&failure.content).unwrap();
            assert_eq!(payload["kind"], kind, "{operation}: {payload}");
            if operation == "vendor.transport" {
                assert_eq!(payload["refusal"], "transport");
                assert_eq!(payload["sent"], "maybe");
                assert_eq!(payload["retryable"], true);
            }
            if operation == "vendor.disconnected" {
                assert_eq!(payload["code"], "disconnected");
            }
            if operation == "vendor.ambiguous" {
                assert_eq!(payload["code"], "ambiguous_connection");
            }
            assert!(!failure.content.contains(SERVICE_TOKEN));
        }

        mode.store(1, Ordering::SeqCst);
        let refresh = ExchangeCatalogRefresher::new(client)
            .refresh(&executor.live_catalog())
            .await;
        assert!(refresh.is_err());
        assert_eq!(
            executor.live_catalog().snapshot().names(),
            vec!["core.echo"]
        );
        assert_eq!(
            executor.dispatch("core.echo", json!({})).await.content,
            "core-ok"
        );
        assert!(executor.dispatch("vendor.ok", json!({})).await.is_error);

        server.abort();
    }

    #[test]
    fn effective_catalogue_rejects_model_selectable_authority() {
        let client = Arc::new(
            ExchangeClient::new(
                "http://127.0.0.1:9",
                "flux-service-account-token-123",
                Redactor::new(),
            )
            .unwrap(),
        );
        let catalogue = EffectiveCatalogue {
            generation: format!("sha256:{}", "0".repeat(64)),
            operations: vec![json!({
                "id": "bad",
                "description": "bad",
                "input_schema": {"type": "object"},
                "effects": ["network"],
                "risk": "low",
                "idempotency": "idempotent",
                "admitted": true,
                "tenant": "model-chosen"
            })],
        };
        let error = match client.project(catalogue) {
            Ok(_) => panic!("model-selectable authority must be rejected"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("forbidden authority"));
    }

    #[test]
    fn cleartext_exchange_origins_are_loopback_only() {
        for allowed in [
            "http://127.0.0.1:9",
            "http://[::1]:9",
            "http://localhost:9",
            "http://dev.localhost:9",
        ] {
            ExchangeClient::new(allowed, "flux-service-account-token-123", Redactor::new())
                .unwrap_or_else(|error| {
                    panic!("{allowed} should be accepted for local development: {error}")
                });
        }

        for refused in [
            "http://example.com",
            "http://192.0.2.1:8080",
            "http://0.0.0.0:8080",
        ] {
            let error =
                ExchangeClient::new(refused, "flux-service-account-token-123", Redactor::new())
                    .err()
                    .unwrap_or_else(|| {
                        panic!("{refused} must not receive a bearer over cleartext")
                    });
            assert!(error.to_string().contains("HTTPS"), "{refused}: {error}");
        }
    }
}
