use std::sync::Arc;

use async_trait::async_trait;
use codewandler_flux_capabilities::{validate_live_contract, LiveAccess, LiveDatasource};
use flux_datasource::live::{
    FilterKey, FilterType, Filters, LiveEntity, LiveSchema, Page, PageRequest, Row,
};
use flux_runtime::ToolContext;
use flux_system::{System, Workspace};

struct FixtureBackend;

#[async_trait]
impl LiveDatasource for FixtureBackend {
    fn schema(&self) -> LiveSchema {
        valid_schema()
    }

    fn access(&self) -> Vec<LiveAccess> {
        vec![LiveAccess::Network {
            subject: "https://tickets.example".into(),
        }]
    }

    async fn list(
        &self,
        _ctx: &ToolContext,
        _entity: &str,
        page: PageRequest,
        _filters: &Filters,
    ) -> flux_core::Result<Page<Row>> {
        Ok(Page {
            rows: Vec::new(),
            next: page.cursor,
        })
    }

    async fn get(
        &self,
        _ctx: &ToolContext,
        _entity: &str,
        _id: &str,
    ) -> flux_core::Result<Option<Row>> {
        Ok(None)
    }
}

fn valid_schema() -> LiveSchema {
    LiveSchema {
        entities: vec![LiveEntity {
            entity: "ticket".into(),
            filters: vec![FilterKey {
                name: "state".into(),
                ty: FilterType::Enum(vec!["open".into(), "closed".into()]),
                required: false,
                description: None,
            }],
            default_page: 20,
            max_page: 100,
            description: None,
        }],
    }
}

#[tokio::test]
async fn trait_is_object_safe_and_receives_guarded_context() {
    let backend: Arc<dyn LiveDatasource> = Arc::new(FixtureBackend);
    validate_live_contract("tickets", &backend.schema(), &backend.access()).unwrap();

    let root = std::env::temp_dir().join(format!(
        "flux-live-trait-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    std::fs::create_dir_all(&root).unwrap();
    let ctx = ToolContext::new(Arc::new(System::new(Workspace::new(&root).unwrap())));
    let page = backend
        .list(
            &ctx,
            "ticket",
            PageRequest {
                cursor: Some("opaque".into()),
                limit: 20,
            },
            &Filters::new(),
        )
        .await
        .unwrap();
    assert_eq!(page.next.as_deref(), Some("opaque"));
}

#[test]
fn validation_rejects_impossible_schemas_and_authority() {
    let mut schema = valid_schema();
    schema.entities[0].default_page = 101;
    assert!(validate_live_contract("tickets", &schema, &[])
        .unwrap_err()
        .to_string()
        .contains("default_page"));

    let mut schema = valid_schema();
    schema.entities.push(schema.entities[0].clone());
    assert!(validate_live_contract("tickets", &schema, &[])
        .unwrap_err()
        .to_string()
        .contains("duplicate entity"));

    assert!(validate_live_contract(
        "tickets",
        &valid_schema(),
        &[LiveAccess::Connection {
            subject: " ".into(),
        }],
    )
    .unwrap_err()
    .to_string()
    .contains("blank connection"));

    assert!(validate_live_contract("bad.domain", &valid_schema(), &[]).is_err());
}
