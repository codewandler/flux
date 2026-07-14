use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use codewandler_flux_capabilities::{try_register_live_datasource, LiveAccess, LiveDatasource};
use flux_datasource::live::{
    FilterKey, FilterType, FilterValue, Filters, LiveEntity, LiveSchema, Page, PageRequest,
    Reference, Row,
};
use flux_evidence::{SignalMatch, ToolGroup, KIND_SIGNAL};
use flux_runtime::{
    tool_fn, AuthorityRequirement, DenyApprover, ExecutionAuthorization, Executor,
    PermissionManager, ToolContext, ToolRegistry,
};
use flux_spec::{AccessKind, Effect, ToolSpec};
use flux_system::{System, Workspace};
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    List {
        system: usize,
        entity: String,
        page: PageRequest,
        filters: Vec<(String, FilterValue)>,
    },
    Get {
        system: usize,
        entity: String,
        id: String,
    },
}

#[derive(Default)]
struct MockBackend {
    calls: Mutex<Vec<Call>>,
    access: Mutex<Vec<LiveAccess>>,
    access_calls: AtomicUsize,
    fail_list: AtomicBool,
    fail_get: AtomicBool,
}

impl MockBackend {
    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    fn with_access(access: Vec<LiveAccess>) -> Self {
        Self {
            access: Mutex::new(access),
            ..Self::default()
        }
    }

    fn set_access(&self, access: Vec<LiveAccess>) {
        *self.access.lock().unwrap() = access;
    }
}

#[async_trait]
impl LiveDatasource for MockBackend {
    fn schema(&self) -> LiveSchema {
        LiveSchema {
            entities: vec![
                LiveEntity {
                    entity: "ticket".into(),
                    filters: vec![
                        FilterKey {
                            name: "state".into(),
                            ty: FilterType::Enum(vec!["open".into(), "closed".into()]),
                            required: false,
                            description: Some("Workflow state".into()),
                        },
                        FilterKey {
                            name: "priority".into(),
                            ty: FilterType::Int,
                            required: false,
                            description: None,
                        },
                        FilterKey {
                            name: "owner".into(),
                            ty: FilterType::String,
                            required: true,
                            description: None,
                        },
                    ],
                    default_page: 20,
                    max_page: 100,
                    description: Some("Support tickets".into()),
                },
                LiveEntity {
                    entity: "user".into(),
                    filters: vec![FilterKey {
                        name: "active".into(),
                        ty: FilterType::Bool,
                        required: false,
                        description: None,
                    }],
                    default_page: 10,
                    max_page: 50,
                    description: None,
                },
            ],
        }
    }

    fn access(&self) -> Vec<LiveAccess> {
        self.access_calls.fetch_add(1, Ordering::SeqCst);
        self.access.lock().unwrap().clone()
    }

    async fn list(
        &self,
        ctx: &ToolContext,
        entity: &str,
        page: PageRequest,
        filters: &Filters,
    ) -> flux_core::Result<Page<Row>> {
        self.calls.lock().unwrap().push(Call::List {
            system: Arc::as_ptr(&ctx.system) as usize,
            entity: entity.into(),
            page,
            filters: filters
                .iter()
                .map(|(name, value)| (name.to_string(), value.clone()))
                .collect(),
        });
        if self.fail_list.load(Ordering::Relaxed) {
            return Err(flux_core::Error::Other("fixture list failed".into()));
        }
        if entity == "user" {
            return Ok(Page {
                rows: Vec::new(),
                next: Some("users-next".into()),
            });
        }
        Ok(Page {
            rows: vec![ticket_row()],
            next: Some("cursor-2".into()),
        })
    }

    async fn get(
        &self,
        ctx: &ToolContext,
        entity: &str,
        id: &str,
    ) -> flux_core::Result<Option<Row>> {
        self.calls.lock().unwrap().push(Call::Get {
            system: Arc::as_ptr(&ctx.system) as usize,
            entity: entity.into(),
            id: id.into(),
        });
        if self.fail_get.load(Ordering::Relaxed) {
            return Err(flux_core::Error::Other("fixture get failed".into()));
        }
        Ok((id == "T-1").then(ticket_row))
    }
}

fn ticket_row() -> Row {
    Row {
        id: "T-1".into(),
        title: "Login broken".into(),
        summary: "Customer cannot sign in".into(),
        reference: Some(Reference::Url {
            url: "https://tickets.example/T-1".into(),
        }),
    }
}

fn ctx() -> ToolContext {
    let root = std::env::temp_dir().join(format!(
        "flux-live-ops-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    std::fs::create_dir_all(&root).unwrap();
    ToolContext::new(Arc::new(System::new(Workspace::new(&root).unwrap())))
}

fn operation(registry: &ToolRegistry, name: &str) -> Arc<dyn flux_runtime::Tool> {
    registry
        .get(name)
        .unwrap_or_else(|| panic!("missing operation {name}"))
}

fn ticket_branch(schema: &Value) -> &Value {
    schema["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .find(|branch| branch["properties"]["entity"]["const"] == "ticket")
        .expect("ticket schema branch")
}

#[test]
fn registration_installs_two_source_labelled_generated_contracts() {
    let mut registry = ToolRegistry::new();
    let surface =
        try_register_live_datasource(&mut registry, "tickets", Arc::new(MockBackend::default()))
            .unwrap();

    assert_eq!(registry.names(), ["tickets.get", "tickets.list"]);
    assert_eq!(
        registry.source("tickets.list"),
        Some("flux-capabilities live datasource `tickets`")
    );
    assert_eq!(
        registry.source("tickets.get"),
        registry.source("tickets.list")
    );

    let list = operation(&registry, "tickets.list").spec();
    let ticket = ticket_branch(&list.input_schema);
    assert_eq!(ticket["properties"]["entity"]["const"], "ticket");
    assert_eq!(
        ticket["properties"]["filters"]["properties"]["state"]["enum"],
        json!(["open", "closed"])
    );
    assert_eq!(
        ticket["properties"]["filters"]["properties"]["priority"]["type"],
        "integer"
    );
    assert_eq!(
        ticket["properties"]["filters"]["required"],
        json!(["owner"])
    );
    let user = list.input_schema["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .find(|branch| branch["properties"]["entity"]["const"] == "user")
        .expect("user schema branch");
    assert_eq!(
        user["properties"]["filters"]["properties"]["active"]["type"],
        "boolean"
    );
    assert!(user.get("description").is_none());
    let get = operation(&registry, "tickets.get").spec();
    assert_eq!(
        get.input_schema["properties"]["entity"]["enum"],
        json!(["ticket", "user"])
    );
    assert_eq!(get.input_schema["required"], json!(["entity", "id"]));
    assert_eq!(surface.ambient_signal, "tickets");
    assert_eq!(
        surface.group,
        ToolGroup {
            name: "tickets".into(),
            description: "Live datasource operations for `tickets`.".into(),
            tools: vec!["tickets.list".into(), "tickets.get".into()],
            surface_when: vec![SignalMatch {
                kind: KIND_SIGNAL.into(),
                signal: Some("tickets".into()),
            }],
        }
    );
}

#[test]
fn generated_operations_surface_only_with_the_registration_signal() {
    let mut registry = ToolRegistry::new();
    let surface =
        try_register_live_datasource(&mut registry, "tickets", Arc::new(MockBackend::default()))
            .unwrap();

    assert!(registry
        .active_specs(std::slice::from_ref(&surface.group), &HashSet::new())
        .is_empty());
    let active = HashSet::from([surface.ambient_signal.clone()]);
    assert_eq!(
        registry
            .active_specs(std::slice::from_ref(&surface.group), &active)
            .into_iter()
            .map(|spec| (spec.name, spec.group))
            .collect::<Vec<_>>(),
        vec![
            ("tickets.get".into(), Some("tickets".into())),
            ("tickets.list".into(), Some("tickets".into())),
        ]
    );
}

#[test]
fn surface_all_override_exposes_generated_operations_without_the_signal() {
    const CHILD: &str = "FLUX_D172_SURFACE_ALL_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let mut registry = ToolRegistry::new();
        let surface = try_register_live_datasource(
            &mut registry,
            "tickets",
            Arc::new(MockBackend::default()),
        )
        .unwrap();
        assert_eq!(
            registry
                .active_specs(std::slice::from_ref(&surface.group), &HashSet::new())
                .into_iter()
                .map(|spec| spec.name)
                .collect::<Vec<_>>(),
            vec!["tickets.get", "tickets.list"]
        );
        return;
    }

    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "surface_all_override_exposes_generated_operations_without_the_signal",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env("FLUX_SURFACE_ALL", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "surface-all child failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn access_is_snapshotted_into_honest_specs_and_exact_invocation_authority() {
    let backend = Arc::new(MockBackend::with_access(vec![
        LiveAccess::Network {
            subject: "https://tickets.example/api".into(),
        },
        LiveAccess::Connection {
            subject: "tcp:tickets-db.example:5432".into(),
        },
    ]));
    let mut registry = ToolRegistry::new();
    try_register_live_datasource(&mut registry, "tickets", backend.clone()).unwrap();
    backend.set_access(vec![LiveAccess::Network {
        subject: "https://changed.example".into(),
    }]);

    assert_eq!(backend.access_calls.load(Ordering::SeqCst), 1);
    let expected_requirements = vec![
        AuthorityRequirement::datasource_read("tickets/ticket"),
        AuthorityRequirement::network_fetch("https://tickets.example/api"),
        AuthorityRequirement::connection_dial("tcp:tickets-db.example:5432"),
    ];
    let cases = [
        (
            "tickets.list",
            json!({
                "entity": "ticket",
                "page": "cursor-containing-secret-material",
                "filters": {"owner": "secret-owner", "state": "open"}
            }),
        ),
        (
            "tickets.list",
            json!({
                "entity": "ticket",
                "page": "different-cursor",
                "filters": {"owner": "different-secret", "state": "closed"}
            }),
        ),
        (
            "tickets.get",
            json!({"entity": "ticket", "id": "opaque-handle-or-secret"}),
        ),
    ];
    for (name, params) in cases {
        let tool = operation(&registry, name);
        let spec = tool.spec();
        assert_eq!(spec.effects, vec![Effect::Read, Effect::Network]);
        assert_eq!(
            spec.access,
            vec![
                AccessKind::Datasource,
                AccessKind::Network,
                AccessKind::Connection,
            ]
        );
        let subjects = tool.permission_subjects(&params);
        assert_eq!(subjects, vec!["tickets/ticket"]);
        assert_eq!(
            tool.authority_requirements(&params, &subjects).unwrap(),
            expected_requirements
        );
    }

    for name in ["tickets.list", "tickets.get"] {
        let tool = operation(&registry, name);
        let params = json!({});
        let subjects = tool.permission_subjects(&params);
        assert_eq!(subjects, vec!["tickets/*"]);
        assert_eq!(
            tool.authority_requirements(&params, &subjects).unwrap(),
            vec![
                AuthorityRequirement::datasource_read("tickets/*"),
                AuthorityRequirement::network_fetch("https://tickets.example/api"),
                AuthorityRequirement::connection_dial("tcp:tickets-db.example:5432"),
            ]
        );
    }

    let list = operation(&registry, "tickets.list");
    let unknown = json!({"entity": "undeclared"});
    let subjects = list.permission_subjects(&unknown);
    assert_eq!(subjects, vec!["tickets/undeclared"]);
    assert_eq!(
        list.authority_requirements(&unknown, &subjects).unwrap()[0],
        AuthorityRequirement::datasource_read("tickets/undeclared")
    );
}

#[tokio::test]
async fn dispatch_enforces_the_snapshotted_exact_backend_authority() {
    let backend = Arc::new(MockBackend::with_access(vec![LiveAccess::Network {
        subject: "https://tickets.example/api".into(),
    }]));
    let mut registry = ToolRegistry::new();
    try_register_live_datasource(&mut registry, "tickets", backend.clone()).unwrap();

    let mut matching_policy = ExecutionAuthorization::local().policy().clone();
    for grant in &mut matching_policy.grants {
        if grant
            .actions
            .iter()
            .any(|action| action.0 == "network.fetch")
        {
            for resource in &mut grant.resources {
                resource.id = "https://tickets.example/api".into();
            }
        }
    }
    let mut mismatching_policy = matching_policy.clone();
    for grant in &mut mismatching_policy.grants {
        if grant
            .actions
            .iter()
            .any(|action| action.0 == "network.fetch")
        {
            for resource in &mut grant.resources {
                resource.id = "https://other.example/api".into();
            }
        }
    }
    let executor = Executor::new(
        registry.clone(),
        PermissionManager::from_rules(&["tickets.list(tickets/ticket)".into()], &[]),
        Arc::new(DenyApprover),
        ctx(),
    )
    .with_policy(mismatching_policy);

    let result = executor
        .dispatch(
            "tickets.list",
            json!({"entity": "ticket", "filters": {"owner": "ops"}}),
        )
        .await;
    assert!(result.is_error);
    assert!(
        result.content.contains("network.fetch"),
        "{}",
        result.content
    );
    assert!(backend.calls().is_empty());

    let matching_executor = Executor::new(
        registry,
        PermissionManager::from_rules(&["tickets.list(tickets/ticket)".into()], &[]),
        Arc::new(DenyApprover),
        ctx(),
    )
    .with_policy(matching_policy);
    let result = matching_executor
        .dispatch(
            "tickets.list",
            json!({"entity": "ticket", "filters": {"owner": "ops"}}),
        )
        .await;
    assert!(!result.is_error, "{}", result.content);
    assert_eq!(backend.calls().len(), 1);
}

#[tokio::test]
async fn list_and_get_route_through_the_guarded_context_and_render_consistently() {
    let backend = Arc::new(MockBackend::default());
    let mut registry = ToolRegistry::new();
    try_register_live_datasource(&mut registry, "tickets", backend.clone()).unwrap();
    let ctx = ctx();
    let system = Arc::as_ptr(&ctx.system) as usize;

    let listed = operation(&registry, "tickets.list")
        .execute(
            &ctx,
            json!({
                "entity": "ticket",
                "page": "opaque:1/β",
                "limit": 7,
                "filters": {"state": "open", "priority": 2, "owner": "ops"}
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        listed.content,
        "[ticket T-1] Login broken — Customer cannot sign in\nnext: cursor-2"
    );

    let got = operation(&registry, "tickets.get")
        .execute(&ctx, json!({"entity": "ticket", "id": "T-1"}))
        .await
        .unwrap();
    assert_eq!(
        got.content,
        "[ticket T-1] Login broken — Customer cannot sign in\nreference: https://tickets.example/T-1"
    );

    assert_eq!(
        backend.calls(),
        vec![
            Call::List {
                system,
                entity: "ticket".into(),
                page: PageRequest {
                    cursor: Some("opaque:1/β".into()),
                    limit: 7,
                },
                filters: vec![
                    ("owner".into(), FilterValue::String("ops".into())),
                    ("priority".into(), FilterValue::Int(2)),
                    ("state".into(), FilterValue::String("open".into())),
                ],
            },
            Call::Get {
                system,
                entity: "ticket".into(),
                id: "T-1".into(),
            },
        ]
    );

    let empty = operation(&registry, "tickets.list")
        .execute(&ctx, json!({"entity": "user"}))
        .await
        .unwrap();
    assert_eq!(empty.content, "no records\nnext: users-next");
    let missing = operation(&registry, "tickets.get")
        .execute(&ctx, json!({"entity": "ticket", "id": "missing"}))
        .await
        .unwrap();
    assert_eq!(missing.content, "not found");
}

#[test]
fn a_collision_rejects_the_pair_without_leaking_the_first_tool() {
    let existing = tool_fn(
        ToolSpec::read_only("tickets.get", "existing", json!({"type": "object"})),
        |_params| async { Ok(json!("existing")) },
    );
    let mut registry = ToolRegistry::new();
    registry.try_register_from("fixture", existing).unwrap();

    let error =
        try_register_live_datasource(&mut registry, "tickets", Arc::new(MockBackend::default()))
            .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("tickets.get"), "{message}");
    assert!(message.contains("fixture"), "{message}");
    assert!(
        message.contains("flux-capabilities live datasource `tickets`"),
        "{message}"
    );
    assert_eq!(registry.names(), ["tickets.get"]);
    assert_eq!(registry.source("tickets.get"), Some("fixture"));
    assert!(registry.get("tickets.list").is_none());
}

#[tokio::test]
async fn backend_errors_propagate_without_becoming_successful_results() {
    let backend = Arc::new(MockBackend::default());
    backend.fail_list.store(true, Ordering::Relaxed);
    backend.fail_get.store(true, Ordering::Relaxed);
    let mut registry = ToolRegistry::new();
    try_register_live_datasource(&mut registry, "tickets", backend).unwrap();
    let ctx = ctx();

    let list_error = operation(&registry, "tickets.list")
        .execute(
            &ctx,
            json!({"entity": "ticket", "filters": {"owner": "ops"}}),
        )
        .await
        .unwrap_err();
    assert!(list_error.to_string().contains("fixture list failed"));

    let get_error = operation(&registry, "tickets.get")
        .execute(&ctx, json!({"entity": "ticket", "id": "T-1"}))
        .await
        .unwrap_err();
    assert!(get_error.to_string().contains("fixture get failed"));
}

#[tokio::test]
async fn invalid_entities_and_filters_fail_with_paths_before_backend_entry() {
    let backend = Arc::new(MockBackend::default());
    let mut registry = ToolRegistry::new();
    try_register_live_datasource(&mut registry, "tickets", backend.clone()).unwrap();
    let ctx = ctx();
    let list = operation(&registry, "tickets.list");

    let cases = [
        (
            json!({"entity": "unknown"}),
            "tickets.list.entity: unknown entity `unknown`",
        ),
        (
            json!({"entity": "ticket", "filters": {"owner": "ops", "extra": true}}),
            "tickets.list.filters.extra: unknown filter for entity `ticket`",
        ),
        (
            json!({"entity": "ticket"}),
            "tickets.list.filters.owner: required filter is missing",
        ),
        (
            json!({"entity": "ticket", "filters": {"owner": false}}),
            "tickets.list.filters.owner: expected string",
        ),
        (
            json!({"entity": "ticket", "filters": {"owner": "ops", "priority": "2"}}),
            "tickets.list.filters.priority: expected integer",
        ),
        (
            json!({"entity": "user", "filters": {"active": "yes"}}),
            "tickets.list.filters.active: expected boolean",
        ),
        (
            json!({"entity": "ticket", "filters": {"owner": "ops", "state": "pending"}}),
            "tickets.list.filters.state: expected one of `open`, `closed`",
        ),
    ];

    for (params, expected) in cases {
        let error = list.execute(&ctx, params).await.unwrap_err().to_string();
        assert!(
            error.contains(expected),
            "expected `{expected}` in `{error}`"
        );
    }

    let get_error = operation(&registry, "tickets.get")
        .execute(&ctx, json!({"entity": "unknown", "id": "1"}))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        get_error.contains("tickets.get.entity: unknown entity `unknown`"),
        "{get_error}"
    );
    assert!(
        backend.calls().is_empty(),
        "rejected inputs must not enter the live backend"
    );
}

#[tokio::test]
async fn limits_filters_and_opaque_cursors_are_normalized_before_backend_entry() {
    let backend = Arc::new(MockBackend::default());
    let mut registry = ToolRegistry::new();
    try_register_live_datasource(&mut registry, "tickets", backend.clone()).unwrap();
    let ctx = ctx();
    let list = operation(&registry, "tickets.list");
    let cursor = "v1:α/β?x=1&y=%2F#雪";

    list.execute(
        &ctx,
        json!({
            "entity": "ticket",
            "page": cursor,
            "filters": {"state": "open", "owner": "ops", "priority": 3}
        }),
    )
    .await
    .unwrap();
    list.execute(&ctx, json!({"entity": "user", "limit": 999}))
        .await
        .unwrap();

    let zero_error = list
        .execute(&ctx, json!({"entity": "user", "limit": 0}))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        zero_error.contains("tickets.list.limit: must be greater than zero"),
        "{zero_error}"
    );

    assert_eq!(
        backend.calls(),
        vec![
            Call::List {
                system: Arc::as_ptr(&ctx.system) as usize,
                entity: "ticket".into(),
                page: PageRequest {
                    cursor: Some(cursor.into()),
                    limit: 20,
                },
                filters: vec![
                    ("owner".into(), FilterValue::String("ops".into())),
                    ("priority".into(), FilterValue::Int(3)),
                    ("state".into(), FilterValue::String("open".into())),
                ],
            },
            Call::List {
                system: Arc::as_ptr(&ctx.system) as usize,
                entity: "user".into(),
                page: PageRequest {
                    cursor: None,
                    limit: 50,
                },
                filters: Vec::new(),
            },
        ]
    );
}
