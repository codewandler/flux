use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use codewandler_flux_capabilities::{try_register_live_datasource, LiveDatasource};
use flux_datasource::live::{
    FilterKey, FilterType, FilterValue, Filters, LiveEntity, LiveSchema, Page, PageRequest,
    Reference, Row,
};
use flux_runtime::{tool_fn, ToolContext, ToolRegistry};
use flux_spec::ToolSpec;
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
    fail_list: AtomicBool,
    fail_get: AtomicBool,
}

impl MockBackend {
    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
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
        .execute(&ctx, json!({"entity": "ticket"}))
        .await
        .unwrap_err();
    assert!(list_error.to_string().contains("fixture list failed"));

    let get_error = operation(&registry, "tickets.get")
        .execute(&ctx, json!({"entity": "ticket", "id": "T-1"}))
        .await
        .unwrap_err();
    assert!(get_error.to_string().contains("fixture get failed"));
}
