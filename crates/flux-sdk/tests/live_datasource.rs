//! Consumer-side proof that a first-class SDK live datasource is discoverable and executable only
//! through the ordinary safety envelope.

#[path = "../examples/support/live_datasource.rs"]
mod support;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use flux_core::{Chunk, ContentBlock, Error, Result, StopReason};
use flux_provider::{ChunkStream, Provider, Request};
use flux_sdk::authorization::{default_local_grants, local_identity};
use flux_sdk::Client;
use serde_json::json;

use support::SupportBackend;

static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> Self {
        let sequence = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "flux-sdk-live-{label}-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create live-datasource test workspace");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

struct NeverProvider;

#[async_trait]
impl Provider for NeverProvider {
    fn name(&self) -> &str {
        "never"
    }

    async fn stream(&self, _request: Request) -> Result<ChunkStream> {
        Err(Error::Other(
            "direct live-datasource dispatch must not invoke the model".into(),
        ))
    }
}

#[derive(Default)]
struct CatalogProvider {
    catalogs: Arc<Mutex<Vec<Vec<String>>>>,
}

impl CatalogProvider {
    fn captured(&self) -> Arc<Mutex<Vec<Vec<String>>>> {
        self.catalogs.clone()
    }
}

#[async_trait]
impl Provider for CatalogProvider {
    fn name(&self) -> &str {
        "catalog"
    }

    async fn stream(&self, request: Request) -> Result<ChunkStream> {
        let intent_stage = request
            .tools
            .iter()
            .any(|tool| tool.name == "declare_intent");
        self.catalogs
            .lock()
            .unwrap()
            .push(request.tools.iter().map(|tool| tool.name.clone()).collect());
        let chunks = if intent_stage {
            vec![
                Chunk::Block(ContentBlock::ToolUse {
                    id: "intent".into(),
                    name: "declare_intent".into(),
                    input: json!({
                        "intent": "inspect support data",
                        "capability_families": ["support"]
                    }),
                }),
                Chunk::Done {
                    stop_reason: Some(StopReason::ToolUse),
                },
            ]
        } else {
            vec![
                Chunk::Block(ContentBlock::Text {
                    text: "catalog captured".into(),
                }),
                Chunk::Done {
                    stop_reason: Some(StopReason::EndTurn),
                },
            ]
        };
        Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
    }
}

#[tokio::test]
async fn dispatches_typed_pages_and_gets_through_the_real_executor() {
    let root = TestDir::new("dispatch");
    let backend = Arc::new(SupportBackend::new());
    let client = Client::builder()
        .model("unused")
        .auto_approve(true)
        .try_with_live_datasource("support", backend.clone())
        .expect("valid support datasource installs")
        .build(Box::new(NeverProvider), root.path())
        .expect("assemble live-datasource client");

    let params = json!({
        "entity": "ticket",
        "limit": 1,
        "filters": {"state": "open", "priority": 2, "escalated": true}
    });
    let list = client
        .engine()
        .executor
        .registry()
        .get("support.list")
        .expect("support.list registered through the SDK convenience seam");
    let subjects = list.permission_subjects(&params);
    assert_eq!(subjects, ["support/ticket"]);
    let requirements = list
        .authority_requirements(&params, &subjects)
        .expect("support invocation has a valid authority contract");
    assert_eq!(
        requirements.len(),
        1,
        "in-memory backend declares no egress"
    );
    assert_eq!(requirements[0].action.0, "datasource.read");
    assert_eq!(requirements[0].resource.id, "support/ticket");

    let first = client
        .engine()
        .executor
        .dispatch("support.list", params)
        .await;
    assert!(!first.is_error, "first list page failed: {}", first.content);
    assert_eq!(
        first.content,
        "[ticket T-100] Checkout retries — Payments retry after a gateway timeout\nnext: v1:ticket:1"
    );

    let second = client
        .engine()
        .executor
        .dispatch(
            "support.list",
            json!({
                "entity": "ticket",
                "page": "v1:ticket:1",
                "limit": 1,
                "filters": {"state": "open", "priority": 2, "escalated": true}
            }),
        )
        .await;
    assert!(
        !second.is_error,
        "second list page failed: {}",
        second.content
    );
    assert_eq!(
        second.content,
        "[ticket T-102] Invoice export delayed — A scheduled export has not completed"
    );

    let customers = client
        .engine()
        .executor
        .dispatch(
            "support.list",
            json!({"entity": "customer", "filters": {"region": "emea"}}),
        )
        .await;
    assert!(
        !customers.is_error,
        "customer list failed: {}",
        customers.content
    );
    assert_eq!(
        customers.content,
        "[customer C-10] Northwind GmbH — Enterprise customer in EMEA"
    );

    let ticket = client
        .engine()
        .executor
        .dispatch("support.get", json!({"entity": "ticket", "id": "T-100"}))
        .await;
    assert!(!ticket.is_error, "ticket get failed: {}", ticket.content);
    assert_eq!(
        ticket.content,
        "[ticket T-100] Checkout retries — Payments retry after a gateway timeout\nreference: customer/C-10"
    );

    let missing = client
        .engine()
        .executor
        .dispatch(
            "support.get",
            json!({"entity": "customer", "id": "missing"}),
        )
        .await;
    assert!(
        !missing.is_error,
        "not-found lookup failed: {}",
        missing.content
    );
    assert_eq!(missing.content, "not found");
    assert_eq!(backend.entries(), 5);
}

#[tokio::test]
async fn configured_domain_surfaces_both_operations_to_the_model_catalog() {
    let root = TestDir::new("catalog");
    let backend = Arc::new(SupportBackend::new());
    let provider = CatalogProvider::default();
    let captured = provider.captured();
    let client = Client::builder()
        .model("mock")
        .try_with_live_datasource("support", backend.clone())
        .expect("valid support datasource installs")
        .build(Box::new(provider), root.path())
        .expect("assemble catalog-capture client");

    let output = client
        .run("Show the configured support operations")
        .await
        .expect("catalog-capture turn succeeds");
    assert_eq!(output.text, "catalog captured");
    let catalogs = captured.lock().unwrap();
    let intent_catalog = catalogs
        .first()
        .expect("intent stage must expose a catalog before capability evidence exists");
    assert!(
        intent_catalog
            .iter()
            .all(|name| !name.starts_with("support_")),
        "support operations surfaced before the support-domain signal: {intent_catalog:?}"
    );
    assert!(
        catalogs.iter().skip(1).any(|catalog| {
            catalog
                .iter()
                .any(|name| name.starts_with("support_list__"))
                && catalog.iter().any(|name| name.starts_with("support_get__"))
        }),
        "configured support operations never surfaced: {catalogs:?}"
    );
    assert_eq!(
        backend.entries(),
        0,
        "catalog discovery must not enter the backend"
    );
}

#[tokio::test]
async fn datasource_policy_denial_happens_before_backend_entry() {
    let root = TestDir::new("policy-denial");
    let backend = Arc::new(SupportBackend::new());
    let mut policy = default_local_grants();
    policy.grants.retain(|grant| {
        !grant
            .actions
            .iter()
            .any(|action| action.0 == "datasource.read")
    });
    let (caller, trust) = local_identity("live-datasource-test");
    let client = Client::builder()
        .model("unused")
        .auto_approve(true)
        .with_authorization(policy, caller, trust)
        .try_with_live_datasource("support", backend.clone())
        .expect("valid support datasource installs")
        .build(Box::new(NeverProvider), root.path())
        .expect("assemble under-granted client");

    let outcome = client
        .engine()
        .executor
        .dispatch_outcome(
            "support.list",
            json!({"entity": "ticket", "filters": {"state": "open"}}),
        )
        .await;
    assert!(outcome.denied, "policy refusal must be structural");
    assert!(outcome.result.is_error);
    assert!(
        outcome.result.content.contains("datasource.read"),
        "{}",
        outcome.result.content
    );
    assert_eq!(
        backend.entries(),
        0,
        "authorization must fail before backend execution"
    );
}
