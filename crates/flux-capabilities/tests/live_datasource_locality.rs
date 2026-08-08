//! C-716: a live datasource connects from where its endpoint is reachable.
//!
//! The three legs of one composition: a `[[host]]` binding says *where* a connection is made from,
//! an endpoint says *what* is connected to (C-709's `host` field is how it says where it is
//! reachable from), and the datasource is the governed read over it. These tests pin the joint —
//! declaration, admission, and which guarded substrate the connection is actually made through.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use codewandler_flux_capabilities::{
    live_connection_system, try_register_live_datasource, LiveAccess, LiveDatasource, LiveLocality,
};
use flux_datasource::live::{
    FilterKey, FilterType, Filters, LiveEntity, LiveSchema, Page, PageRequest, Row,
};
use flux_runtime::{OperationPlacement, ToolContext, ToolRegistry};
use flux_secret::endpoint::EndpointRef;
use flux_system::{System, Workspace};
use serde_json::json;

/// A backend that records the workspace of whatever guarded substrate its connection was routed
/// through, so "connected from the coordinator" and "connected from the selected host" are
/// distinguishable in an assertion rather than by reading the implementation.
struct RecordingBackend {
    access: Vec<LiveAccess>,
    connected_from: Mutex<Vec<String>>,
}

impl RecordingBackend {
    fn new(access: Vec<LiveAccess>) -> Self {
        Self {
            access,
            connected_from: Mutex::new(Vec::new()),
        }
    }

    fn connected_from(&self) -> Vec<String> {
        self.connected_from.lock().unwrap().clone()
    }

    fn connect(&self, ctx: &ToolContext) -> flux_core::Result<()> {
        let system = live_connection_system(ctx, "db", &self.access)?;
        let identity = system.substrate_identity();
        self.connected_from
            .lock()
            .unwrap()
            .push(format!("{}:{}", identity.kind, identity.workspace));
        Ok(())
    }
}

#[async_trait]
impl LiveDatasource for RecordingBackend {
    fn schema(&self) -> LiveSchema {
        LiveSchema {
            entities: vec![LiveEntity {
                entity: "row".into(),
                filters: vec![FilterKey {
                    name: "state".into(),
                    ty: FilterType::String,
                    required: false,
                    description: None,
                }],
                default_page: 10,
                max_page: 50,
                description: None,
            }],
        }
    }

    fn access(&self) -> Vec<LiveAccess> {
        self.access.clone()
    }

    async fn list(
        &self,
        ctx: &ToolContext,
        _entity: &str,
        _page: PageRequest,
        _filters: &Filters,
    ) -> flux_core::Result<Page<Row>> {
        self.connect(ctx)?;
        Ok(Page {
            rows: Vec::new(),
            next: None,
        })
    }

    async fn get(
        &self,
        ctx: &ToolContext,
        _entity: &str,
        _id: &str,
    ) -> flux_core::Result<Option<Row>> {
        self.connect(ctx)?;
        Ok(None)
    }
}

fn workspace(tag: &str) -> Arc<System> {
    let root = std::env::temp_dir().join(format!(
        "flux-live-locality-{}-{tag}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    std::fs::create_dir_all(&root).unwrap();
    Arc::new(System::new(Workspace::new(&root).unwrap()))
}

/// A non-native substrate standing in for the machine a `[[host]]` binding names. Its workspace
/// differs from the coordinator's, so an accidental local connection is visible.
fn selected_substrate(tag: &str) -> (Arc<dyn flux_system::port::ExecutionSystem>, String) {
    let native = workspace(tag);
    let workspace_root = native.workspace().root().display().to_string();
    let selected: Arc<dyn flux_system::port::ExecutionSystem> =
        Arc::new(flux_system::remote::RemoteSystem::loopback(native));
    (selected, workspace_root)
}

fn registered(
    backend: Arc<RecordingBackend>,
) -> (
    ToolRegistry,
    Arc<dyn flux_runtime::Tool>,
    Arc<dyn flux_runtime::Tool>,
) {
    let mut registry = ToolRegistry::new();
    try_register_live_datasource(&mut registry, "db", backend).unwrap();
    let list = registry.get("db.list").expect("db.list");
    let get = registry.get("db.get").expect("db.get");
    (registry, list, get)
}

#[tokio::test]
async fn a_host_bound_endpoint_connects_through_that_hosts_substrate() {
    let backend = Arc::new(RecordingBackend::new(vec![LiveAccess::connection(
        "tcp:db.default.svc.cluster.local:5432",
    )
    .from_host("prod-cluster")]));
    let (_registry, list, _get) = registered(backend.clone());

    let (selected, selected_root) = selected_substrate("prod");
    let ctx = ToolContext::new(workspace("coordinator"))
        .with_execution_system(selected)
        .with_execution_binding("prod-cluster");

    list.execute(&ctx, json!({"entity": "row"})).await.unwrap();

    assert_eq!(
        backend.connected_from(),
        vec![format!("loopback/native:{selected_root}")],
        "a host-bound endpoint's connection must be made from the substrate its host names"
    );
}

#[tokio::test]
async fn a_host_the_session_cannot_select_is_refused_naming_both() {
    let backend = Arc::new(RecordingBackend::new(vec![LiveAccess::connection(
        "tcp:db.default.svc.cluster.local:5432",
    )
    .from_host("prod-cluster")]));
    let (_registry, list, get) = registered(backend.clone());

    // A different binding is selected: the refusal names the host the endpoint needs AND the one
    // the session actually has.
    let (selected, _) = selected_substrate("staging");
    let mismatched = ToolContext::new(workspace("coordinator-mismatch"))
        .with_execution_system(selected)
        .with_execution_binding("staging-cluster");
    let error = list
        .execute(&mismatched, json!({"entity": "row"}))
        .await
        .expect_err("a datasource may not connect from a host the session did not select")
        .to_string();
    assert!(error.contains("prod-cluster"), "{error}");
    assert!(error.contains("staging-cluster"), "{error}");
    assert!(error.contains("db"), "{error}");

    // No selection at all is the same refusal, and it still names the host that is missing.
    let unselected = ToolContext::new(workspace("coordinator-unselected"));
    let error = get
        .execute(&unselected, json!({"entity": "row", "id": "1"}))
        .await
        .expect_err("an unselected session cannot reach a host-local endpoint")
        .to_string();
    assert!(error.contains("prod-cluster"), "{error}");

    assert!(
        backend.connected_from().is_empty(),
        "the refusal is an admission decision — the backend must never be entered"
    );
}

#[tokio::test]
async fn an_endpoint_with_no_host_behaves_exactly_as_today() {
    let backend = Arc::new(RecordingBackend::new(vec![LiveAccess::network(
        "https://tickets.example",
    )]));
    let (registry, list, _get) = registered(backend.clone());

    // Unchanged placement: a datasource that needs no particular vantage point stays native-only.
    assert_eq!(
        registry.declared_placement("db.list"),
        Some(OperationPlacement::NativeSystemOnly)
    );

    let native = workspace("no-host");
    let root = native.workspace().root().display().to_string();
    let ctx = ToolContext::new(native);
    list.execute(&ctx, json!({"entity": "row"})).await.unwrap();
    assert_eq!(backend.connected_from(), vec![format!("native:{root}")]);
}

#[test]
fn a_host_bound_datasource_is_valid_under_a_selection() {
    let backend = Arc::new(RecordingBackend::new(vec![LiveAccess::network(
        "https://api.internal",
    )
    .from_host("prod-cluster")]));
    let mut registry = ToolRegistry::new();
    try_register_live_datasource(&mut registry, "db", backend).unwrap();
    assert_eq!(
        registry.declared_placement("db.list"),
        Some(OperationPlacement::SelectedExecutionSystem),
        "an op that must run from a named host is not native-only — hiding it would refuse without \
         naming the host"
    );
}

#[test]
fn an_endpoints_host_becomes_the_declared_locality() {
    let mut endpoint = EndpointRef::named("pg-prod", "postgres://db.default.svc:5432/app");
    assert_eq!(
        LiveAccess::connection("tcp:db.default.svc:5432")
            .from_endpoint(&endpoint)
            .locality(),
        &LiveLocality::Anywhere,
        "an endpoint with no host stays reachable from wherever the caller is"
    );

    endpoint.host = Some("prod-cluster".into());
    let access = LiveAccess::connection("tcp:db.default.svc:5432").from_endpoint(&endpoint);
    assert_eq!(
        access.locality(),
        &LiveLocality::Host("prod-cluster".into())
    );
    assert_eq!(access.required_host(), Some("prod-cluster"));
}
