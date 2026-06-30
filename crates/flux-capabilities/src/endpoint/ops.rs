//! The agent-facing endpoint ops over the cross-plugin [`EndpointBroker`] + [`EndpointRegistry`]
//! (D-28): `endpoint.discover` / `endpoint.list` / `endpoint.info` / `endpoint.select`.
//!
//! Each is a read-only [`Tool`]. They are the planner's entry point into the discovery spine: ask
//! *"which endpoints exist for product X?"*, inspect the registry, and **select** a weak
//! [`EndpointRef`](flux_secret::endpoint::EndpointRef) to bind to a `$var` and reuse across turns.
//! Everything the agent sees is a weak reference — URLs + display labels + a credential *location*,
//! **never** a secret value (the host injects credentials only at the moment of an IO call, behind
//! `Executor::dispatch`). This mirrors the [`register_datasource_ops`](super::register_datasource_ops)
//! precedent.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use flux_core::{Error, Result};
use flux_runtime::{Tool, ToolContext, ToolRegistry, ToolResult};
use flux_secret::endpoint::EndpointRecord;
use flux_spec::ToolSpec;

use super::{EndpointBroker, EndpointRegistry};

/// The group all four endpoint ops belong to (surfaced by the `kubernetes` signal — see
/// `flux-tools`' `builtin_groups`). Shared so the op specs and the group manifest can't drift.
pub const ENDPOINT_GROUP: &str = "endpoint";

/// The four endpoint ops over `broker` + `endpoints`, as a tool vec (the form a surface registers
/// into an agent/app registry — e.g. `App::with_tools`).
pub fn endpoint_tools(
    broker: Arc<EndpointBroker>,
    endpoints: Arc<EndpointRegistry>,
) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(DiscoverOp(broker)) as Arc<dyn Tool>,
        Arc::new(ListOp(endpoints.clone())),
        Arc::new(InfoOp(endpoints.clone())),
        Arc::new(SelectOp(endpoints)),
    ]
}

/// Register all four endpoint ops over `broker` + `endpoints` into `registry`.
pub fn register_endpoint_ops(
    registry: &mut ToolRegistry,
    broker: Arc<EndpointBroker>,
    endpoints: Arc<EndpointRegistry>,
) {
    for tool in endpoint_tools(broker, endpoints) {
        registry.register(tool);
    }
}

/// A required, non-empty string field.
fn req_str(op: &str, params: &Value, key: &str) -> Result<String> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string())
        .ok_or_else(|| Error::Other(format!("{op}: `{key}` (non-empty string) required")))
}

/// `[product] @endpoint/id url (protocol) [owner/health] labels` — a one-line weak-ref summary.
fn render_record(r: &EndpointRecord) -> String {
    let ep = &r.endpoint;
    let mut out = format!("[{}] {} {}", ep.product, ep.id, ep.url);
    if let Some(proto) = &ep.protocol {
        out.push_str(&format!(" ({proto})"));
    }
    out.push_str(&format!(" owner={}", r.owner));
    if let Some(h) = &r.health {
        out.push_str(&format!(" health={h}"));
    }
    if ep.credential_ref.is_some() {
        // The *presence* of a credential location is useful context — never the value.
        out.push_str(" [credential: host-injected]");
    }
    if !ep.labels.is_empty() {
        let labels: Vec<String> = ep.labels.iter().map(|(k, v)| format!("{k}={v}")).collect();
        out.push_str(&format!(" {{{}}}", labels.join(", ")));
    }
    out
}

/// `endpoint.discover` — fan a product query out to the discovery providers and return weak refs.
struct DiscoverOp(Arc<EndpointBroker>);

#[async_trait]
impl Tool for DiscoverOp {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "endpoint.discover",
            "Discover live service endpoints for a product by fanning out to provider plugins (e.g. \
             kubernetes). Returns weak references — a URL + display labels, never a secret; the host \
             injects credentials when you connect. Product hints: a namespace / cluster / k8s service \
             ⇒ product=\"kubernetes\"; an RDS / SQL database ⇒ product=\"postgres\" (or \"mysql\"); \
             monitoring ⇒ \"prometheus\" / \"loki\" / \"grafana\" / \"alertmanager\". For the latest \
             namespace, put \"latest\" in `query` (the provider picks the newest namespace by creation \
             time). Then bind a result with endpoint.select to reuse it across turns.",
            json!({
                "type": "object",
                "properties": {
                    "product": {"type": "string", "description": "Product class to discover (kubernetes, postgres, mysql, prometheus, loki, grafana, alertmanager)"},
                    "query": {"type": "string", "description": "Free-text hint (e.g. a namespace name, a service name, or \"latest\" for the newest namespace)"},
                    "limit": {"type": "integer", "description": "Max candidates to return (default all)"}
                },
                "required": ["product"]
            }),
        )
        .with_group(ENDPOINT_GROUP)
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        params
            .get("product")
            .and_then(|v| v.as_str())
            .map(|s| vec![s.to_string()])
            .unwrap_or_default()
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let product = req_str("endpoint.discover", &params, "product")?;
        let query = params.get("query").cloned().unwrap_or(Value::Null);
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(usize::MAX);
        // No `requester` — this is the agent (a host op), not a consumer plugin, so the broker fans
        // out to every matching provider.
        let candidates = self.0.discover(&product, &query, limit, None).await;
        if candidates.is_empty() {
            return Ok(ToolResult::ok(format!(
                "no endpoints discovered for product `{product}`"
            )));
        }
        // The committed records carry the owner/health the candidates lack; render the candidates'
        // own weak refs (score-ranked) so the agent sees ranking + reasons.
        let lines: Vec<String> = candidates
            .iter()
            .map(|c| {
                let ep = &c.endpoint;
                let mut line = format!(
                    "[{}] {} {} (score {:.2})",
                    ep.product, ep.id, ep.url, c.score
                );
                if let Some(proto) = &ep.protocol {
                    line.push_str(&format!(" {proto}"));
                }
                if ep.credential_ref.is_some() {
                    line.push_str(" [credential: host-injected]");
                }
                if !c.reasons.is_empty() {
                    line.push_str(&format!(" — {}", c.reasons.join("; ")));
                }
                line
            })
            .collect();
        Ok(ToolResult::ok(lines.join("\n")))
    }
}

/// `endpoint.list` — the registry's discovered + config-bound records (weak refs + owner/health).
struct ListOp(Arc<EndpointRegistry>);

#[async_trait]
impl Tool for ListOp {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "endpoint.list",
            "List the endpoint references currently known to this session (everything discovered or \
             config-bound so far), with owner and last health. Weak references only — no secrets.",
            json!({"type": "object", "properties": {}}),
        )
        .with_group(ENDPOINT_GROUP)
    }

    async fn execute(&self, _ctx: &ToolContext, _params: Value) -> Result<ToolResult> {
        let records = self.0.list();
        if records.is_empty() {
            return Ok(ToolResult::ok(
                "no endpoints known yet — run endpoint.discover first",
            ));
        }
        Ok(ToolResult::ok(
            records
                .iter()
                .map(render_record)
                .collect::<Vec<_>>()
                .join("\n"),
        ))
    }
}

/// `endpoint.info` — one registry record by id.
struct InfoOp(Arc<EndpointRegistry>);

#[async_trait]
impl Tool for InfoOp {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "endpoint.info",
            "Show one endpoint reference in full by its id (e.g. \"@endpoint/monitoring-prometheus\"). \
             Weak reference only — URL, product, protocol, labels, owner, health; never a secret.",
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "Endpoint id (e.g. \"@endpoint/<ns>-<name>\")"}
                },
                "required": ["id"]
            }),
        )
        .with_group(ENDPOINT_GROUP)
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let id = req_str("endpoint.info", &params, "id")?;
        match self.0.resolve(&id) {
            Some(r) => Ok(ToolResult::ok(render_record(&r))),
            None => Ok(ToolResult::ok(format!("no endpoint `{id}`"))),
        }
    }
}

/// `endpoint.select` — return the chosen weak [`EndpointRef`](flux_secret::endpoint::EndpointRef) as
/// JSON, for the planner to bind to a `$var` and reuse (no hidden state — it is a normal value).
struct SelectOp(Arc<EndpointRegistry>);

#[async_trait]
impl Tool for SelectOp {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "endpoint.select",
            "Select a discovered endpoint by id and return its weak reference (URL + credential \
             location, never the secret). Bind the returned reference to a variable and pass it to a \
             plugin op / IO call — the host resolves the reference and injects the credential when the \
             call runs. Use this to reuse one endpoint across turns.",
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "Endpoint id from endpoint.discover / endpoint.list"}
                },
                "required": ["id"]
            }),
        )
        .with_group(ENDPOINT_GROUP)
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let id = req_str("endpoint.select", &params, "id")?;
        let record = self
            .0
            .resolve(&id)
            .ok_or_else(|| Error::Other(format!("endpoint.select: no endpoint `{id}`")))?;
        // The weak `EndpointRef` is model-safe by construction (its `credential_ref` is a location,
        // never a value). Serialize it as the op's structured value.
        let value = serde_json::to_string(&record.endpoint)
            .map_err(|e| Error::Other(format!("endpoint.select: {e}")))?;
        Ok(ToolResult::ok(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoint::{EndpointRegistry, PluginRegistry, ProviderInvoker};
    use flux_secret::endpoint::{EndpointCandidate, EndpointRef};
    use flux_secret::Ref;
    use flux_system::{System, Workspace};
    use serde_json::Value as JsonValue;

    fn ctx() -> ToolContext {
        let dir = std::env::temp_dir().join(format!("flux-ep-ops-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap())))
    }

    /// A fake invoker returning one credential-bearing postgres candidate for any product, so the
    /// discover op can be exercised without a provider subprocess.
    struct OnePg;

    #[async_trait]
    impl ProviderInvoker for OnePg {
        async fn discover(
            &self,
            _name: &str,
            _product: &str,
            _query: &JsonValue,
            _limit: usize,
        ) -> std::result::Result<Vec<EndpointCandidate>, String> {
            Ok(vec![EndpointCandidate {
                endpoint: EndpointRef {
                    credential_ref: Some(Ref::kubernetes("prod", "rds-creds", "password")),
                    protocol: Some("postgres".into()),
                    ..EndpointRef::discovered(
                        "prod-orders",
                        "postgres://orders.prod.svc:5432",
                        "postgres",
                    )
                },
                score: 0.9,
                reasons: vec!["secret name matches rds pattern".into()],
            }])
        }
    }

    async fn broker_with_pg() -> (Arc<EndpointBroker>, Arc<EndpointRegistry>) {
        let system = Arc::new(System::new(Workspace::new(std::env::temp_dir()).unwrap()));
        let host = flux_plugin::PluginHost::spawn(&system, "cat", &[])
            .await
            .expect("spawn idle test host");
        let registry = Arc::new(PluginRegistry::new());
        registry.register(
            "kubernetes",
            crate::endpoint::ProviderEntry {
                manifest: Arc::new(flux_plugin::PluginManifest {
                    name: "kubernetes".into(),
                    discovers: vec!["postgres".into()],
                    ..Default::default()
                }),
                host: Arc::new(tokio::sync::Mutex::new(host)),
                caps: Arc::new(flux_plugin::DenyHostCaps),
            },
        );
        let endpoints = Arc::new(EndpointRegistry::new());
        let broker = Arc::new(EndpointBroker::new(
            Arc::new(OnePg),
            registry,
            endpoints.clone(),
        ));
        (broker, endpoints)
    }

    #[tokio::test]
    async fn discover_returns_weak_refs_and_never_a_secret() {
        let (broker, _endpoints) = broker_with_pg().await;
        let op = DiscoverOp(broker);
        let r = op
            .execute(&ctx(), json!({ "product": "postgres", "query": "orders" }))
            .await
            .unwrap();
        assert!(!r.is_error);
        assert!(
            r.content.contains("@endpoint/prod-orders"),
            "got: {}",
            r.content
        );
        assert!(r.content.contains("postgres://orders.prod.svc:5432"));
        // The credential is only flagged as host-injected — never the value or the raw ref string.
        assert!(r.content.contains("host-injected"));
        assert!(!r.content.to_lowercase().contains("password"));
    }

    #[tokio::test]
    async fn select_returns_a_bindable_weak_ref() {
        let (broker, endpoints) = broker_with_pg().await;
        // Discover commits the candidate to the registry; then select returns its weak ref.
        DiscoverOp(broker)
            .execute(&ctx(), json!({ "product": "postgres" }))
            .await
            .unwrap();
        let select = SelectOp(endpoints.clone());
        let r = select
            .execute(&ctx(), json!({ "id": "@endpoint/prod-orders" }))
            .await
            .unwrap();
        // The returned value is a serialized weak EndpointRef: it carries the credential LOCATION
        // (kubernetes/prod/rds-creds/password), never a value.
        let ref_json: EndpointRef = serde_json::from_str(&r.content).unwrap();
        assert_eq!(ref_json.id, "@endpoint/prod-orders");
        assert_eq!(
            ref_json.credential_ref.unwrap(),
            Ref::kubernetes("prod", "rds-creds", "password")
        );
        // An unknown id is an error.
        assert!(select
            .execute(&ctx(), json!({ "id": "@endpoint/nope" }))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn list_and_info_render_committed_records() {
        let (broker, endpoints) = broker_with_pg().await;
        DiscoverOp(broker)
            .execute(&ctx(), json!({ "product": "postgres" }))
            .await
            .unwrap();
        let list = ListOp(endpoints.clone());
        let l = list.execute(&ctx(), json!({})).await.unwrap();
        assert!(l.content.contains("@endpoint/prod-orders"));
        assert!(l.content.contains("owner=kubernetes"));

        let info = InfoOp(endpoints);
        let i = info
            .execute(&ctx(), json!({ "id": "@endpoint/prod-orders" }))
            .await
            .unwrap();
        assert!(i.content.contains("postgres://orders.prod.svc:5432"));
        assert!(i.content.contains("(postgres)"));
        assert!(!i.content.to_lowercase().contains("password"));
    }

    #[test]
    fn ops_declare_the_endpoint_group() {
        let endpoints = Arc::new(EndpointRegistry::new());
        // SelectOp/ListOp/InfoOp are constructible without a broker.
        for tool in [
            Arc::new(ListOp(endpoints.clone())) as Arc<dyn Tool>,
            Arc::new(InfoOp(endpoints.clone())),
            Arc::new(SelectOp(endpoints)),
        ] {
            let spec = tool.spec();
            assert_eq!(spec.group.as_deref(), Some(ENDPOINT_GROUP));
            // Read-only: the only effect is Read; nothing mutating.
            assert!(spec.has_effect(flux_spec::Effect::Read));
            assert!(!spec.has_effect(flux_spec::Effect::Write));
            assert!(!spec.has_effect(flux_spec::Effect::Process));
        }
    }
}
