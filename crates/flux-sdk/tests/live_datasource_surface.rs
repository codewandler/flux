//! D-172: the SDK live-datasource convenience seam keeps registration, surfacing, and typed
//! authority coupled through client assembly.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use flux_core::{Chunk, ContentBlock, Result, StopReason};
use flux_policy::{AuthorizationPolicy, Grant, SubjectKind, SubjectRef, TrustLevel};
use flux_provider::{ChunkStream, Provider, Request};
use flux_runtime::AuthorityRequirement;
use flux_sdk::authorization::local_identity;
use flux_sdk::datasource::{
    FilterKey, FilterType, Filters, LiveAccess, LiveDatasource, LiveEntity, LiveSchema, Page,
    PageRequest, Row,
};
use flux_sdk::observe::{SignalMatch, ToolGroup, KIND_SIGNAL};
use flux_sdk::tools::{Tool, ToolContext, ToolResult, ToolSpec};
use flux_sdk::{dsl, Client};
use serde_json::{json, Value};

const NETWORK_SUBJECT: &str = "https://tickets.example/api";
const CONNECTION_SUBJECT: &str = "tcp:tickets.example:443";

static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new(label: &str) -> Self {
        let sequence = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "flux-sdk-{label}-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create hermetic SDK test workspace");
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

#[derive(Default)]
struct MockLiveDatasource {
    list_calls: AtomicUsize,
}

impl MockLiveDatasource {
    fn list_calls(&self) -> usize {
        self.list_calls.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl LiveDatasource for MockLiveDatasource {
    fn schema(&self) -> LiveSchema {
        LiveSchema {
            entities: vec![LiveEntity {
                entity: "ticket".into(),
                filters: vec![FilterKey {
                    name: "owner".into(),
                    ty: FilterType::String,
                    required: false,
                    description: Some("Ticket owner".into()),
                }],
                default_page: 20,
                max_page: 100,
                description: Some("Support tickets".into()),
            }],
        }
    }

    fn access(&self) -> Vec<LiveAccess> {
        vec![
            LiveAccess::Network {
                subject: NETWORK_SUBJECT.into(),
            },
            LiveAccess::Connection {
                subject: CONNECTION_SUBJECT.into(),
            },
        ]
    }

    async fn list(
        &self,
        _ctx: &ToolContext,
        _entity: &str,
        _page: PageRequest,
        _filters: &Filters,
    ) -> Result<Page<Row>> {
        self.list_calls.fetch_add(1, Ordering::Relaxed);
        Ok(Page {
            rows: Vec::new(),
            next: None,
        })
    }

    async fn get(&self, _ctx: &ToolContext, _entity: &str, _id: &str) -> Result<Option<Row>> {
        Ok(None)
    }
}

struct StaticTool {
    name: &'static str,
    group: Option<&'static str>,
}

#[async_trait]
impl Tool for StaticTool {
    fn spec(&self) -> ToolSpec {
        let spec = ToolSpec::read_only(
            self.name,
            "SDK live-datasource integration-test probe",
            json!({"type": "object", "properties": {}}),
        );
        match self.group {
            Some(group) => spec.with_group(group),
            None => spec,
        }
    }

    async fn execute(&self, _ctx: &ToolContext, _params: Value) -> Result<ToolResult> {
        Ok(ToolResult::ok("ok"))
    }
}

#[derive(Default)]
struct CatalogCapture {
    requests: Arc<Mutex<Vec<String>>>,
}

impl CatalogCapture {
    fn captured(&self) -> Arc<Mutex<Vec<String>>> {
        self.requests.clone()
    }
}

#[async_trait]
impl Provider for CatalogCapture {
    fn name(&self) -> &str {
        "catalog-capture"
    }

    async fn stream(&self, request: Request) -> Result<ChunkStream> {
        let declares_intent = request
            .tools
            .iter()
            .any(|tool| tool.name == "declare_intent");
        let rendered = request
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        self.requests.lock().unwrap().push(rendered);

        let chunks = if declares_intent {
            vec![
                Chunk::Block(ContentBlock::ToolUse {
                    id: "intent".into(),
                    name: "declare_intent".into(),
                    input: json!({
                        "intent": "inspect the configured catalog",
                        "capability_families": ["tickets", "sdk_test_probe"]
                    }),
                }),
                Chunk::Done {
                    stop_reason: Some(StopReason::ToolUse),
                },
            ]
        } else {
            vec![
                Chunk::Block(ContentBlock::Text { text: "ok".into() }),
                Chunk::Done {
                    stop_reason: Some(StopReason::EndTurn),
                },
            ]
        };
        Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
    }
}

struct NeverProvider;

#[async_trait]
impl Provider for NeverProvider {
    fn name(&self) -> &str {
        "never"
    }

    async fn stream(&self, _request: Request) -> Result<ChunkStream> {
        panic!("direct live-datasource dispatch must not invoke the model")
    }
}

fn probe_group() -> ToolGroup {
    ToolGroup {
        name: "sdk_test_probe".into(),
        description: "Unrelated caller-owned group".into(),
        tools: vec!["zz_d172_probe".into()],
        surface_when: vec![SignalMatch {
            kind: KIND_SIGNAL.into(),
            signal: Some("sdk_test_probe_ready".into()),
        }],
    }
}

fn expected_requirements() -> Vec<AuthorityRequirement> {
    vec![
        AuthorityRequirement::datasource_read("tickets/ticket"),
        AuthorityRequirement::network_fetch(NETWORK_SUBJECT),
        AuthorityRequirement::connection_dial(CONNECTION_SUBJECT),
    ]
}

fn exact_policy(requirements: &[AuthorityRequirement]) -> AuthorizationPolicy {
    AuthorizationPolicy {
        grants: requirements
            .iter()
            .map(|requirement| Grant {
                subjects: vec![SubjectRef {
                    kind: SubjectKind::User,
                    id: "sdk-test-user".into(),
                }],
                resources: vec![requirement.resource.clone()],
                actions: vec![requirement.action.clone()],
                required_trust: TrustLevel::Untrusted,
                required_scopes: Vec::new(),
                requires_approval: false,
            })
            .collect(),
    }
}

#[tokio::test]
async fn live_surface_survives_later_group_and_ambient_signal_setters() {
    let root = TestDir::new("live-surface");
    let provider = CatalogCapture::default();
    let captured = provider.captured();

    let client = Client::builder()
        .model("mock")
        .try_with_live_datasource("tickets", Arc::new(MockLiveDatasource::default()))
        .expect("valid live datasource installs")
        .register_op(Arc::new(StaticTool {
            name: "zz_d172_probe",
            group: Some("sdk_test_probe"),
        }))
        // These replacement-style setters deliberately run after the convenience seam. The live
        // group and signal must be merged at build rather than silently clobbered here.
        .groups([probe_group()])
        .ambient_signals(["sdk_test_probe_ready"])
        .build(Box::new(provider), root.path())
        .expect("assemble client with live surface");

    assert!(
        client.engine().groups.iter().any(|group| {
            group.name == "tickets"
                && group.tools == ["tickets.list", "tickets.get"]
                && group.surface_when
                    == [SignalMatch {
                        kind: KIND_SIGNAL.into(),
                        signal: Some("tickets".into()),
                    }]
        }),
        "the per-domain group must survive a later groups() call: {:?}",
        client
            .engine()
            .groups
            .iter()
            .map(|group| group.name.as_str())
            .collect::<Vec<_>>()
    );
    assert!(
        client
            .engine()
            .groups
            .iter()
            .any(|group| group.name == "sdk_test_probe"),
        "the caller-owned group must survive the live-surface merge"
    );

    client
        .run("Which operations are configured?")
        .await
        .expect("catalog-capture turn succeeds");
    let catalog = captured.lock().unwrap().join("\n");
    for operation in ["tickets_list__", "tickets_get__", "zz_d172_probe"] {
        assert!(
            catalog.contains(operation),
            "provider alias `{operation}` must surface when both the live and caller ambient signals survive:\n{catalog}"
        );
    }
}

#[test]
fn live_surface_preserves_source_aware_duplicate_diagnostics_at_build() {
    let root = TestDir::new("live-duplicate");
    let error = Client::builder()
        .try_with_live_datasource("tickets", Arc::new(MockLiveDatasource::default()))
        .expect("the isolated live pack is valid before composition")
        .register_op_from(
            "sdk-test conflicting pack",
            Arc::new(StaticTool {
                name: "tickets.list",
                group: None,
            }),
        )
        .build(Box::new(NeverProvider), root.path())
        .err()
        .expect("the duplicate operation must fail client assembly")
        .to_string();

    assert!(
        error.contains("duplicate operation `tickets.list`"),
        "{error}"
    );
    assert!(error.contains("sdk-test conflicting pack"), "{error}");
    assert!(
        error.contains("flux-capabilities live datasource `tickets`"),
        "{error}"
    );
}

#[tokio::test]
async fn plan_preview_and_dispatch_share_the_exact_live_authority_contract() {
    let root = TestDir::new("live-authority");
    let backend = Arc::new(MockLiveDatasource::default());
    let expected = expected_requirements();
    let (caller, trust) = local_identity("sdk-test-user");
    let client = Client::builder()
        .model("mock")
        .auto_approve(true)
        .with_authorization(exact_policy(&expected), caller, trust)
        .try_with_live_datasource("tickets", backend.clone())
        .expect("valid live datasource installs")
        .build(Box::new(NeverProvider), root.path())
        .expect("assemble exact-authority client");

    let params = json!({
        "entity": "ticket",
        "page": "cursor:secret:env/PAGING_TOKEN",
        "limit": 5,
        "filters": {"owner": "secret:env/TICKET_OWNER"}
    });
    let list = client
        .engine()
        .executor
        .registry()
        .get("tickets.list")
        .expect("SDK convenience seam registered tickets.list");
    let subjects = list.permission_subjects(&params);
    assert_eq!(subjects, ["tickets/ticket"]);
    let dispatch_contract = list
        .authority_requirements(&params, &subjects)
        .expect("live invocation authority is valid");

    let flow = dsl::DraftAst {
        body: vec![dsl::call("tickets.list", [dsl::lit(params.clone())])],
        ..Default::default()
    };
    let preview = flux_flow::runtime::plan_risk(&flow, client.engine().executor.registry());
    assert_eq!(preview.requirements, expected);
    assert_eq!(preview.requirements, dispatch_contract);
    assert!(
        preview.requirements.iter().all(|requirement| {
            !requirement.resource.id.contains("PAGING_TOKEN")
                && !requirement.resource.id.contains("TICKET_OWNER")
        }),
        "cursor and filter payloads must never become authority subjects"
    );

    let result = client
        .engine()
        .executor
        .dispatch("tickets.list", params.clone())
        .await;
    assert!(
        !result.is_error,
        "exact grants must dispatch: {}",
        result.content
    );
    assert_eq!(backend.list_calls(), 1);

    // Omitting the exact connection grant must make dispatch fail before the backend runs. Together
    // with the successful exact-policy call above, this proves dispatch consumes the same complete
    // requirement set shown by preview rather than merely exposing it as advisory metadata.
    let denied_backend = Arc::new(MockLiveDatasource::default());
    let (caller, trust) = local_identity("sdk-test-user");
    let denied = Client::builder()
        .model("mock")
        .auto_approve(true)
        .with_authorization(exact_policy(&expected[..2]), caller, trust)
        .try_with_live_datasource("tickets", denied_backend.clone())
        .expect("valid live datasource installs")
        .build(Box::new(NeverProvider), root.path())
        .expect("assemble deliberately under-granted client")
        .engine()
        .executor
        .dispatch("tickets.list", params)
        .await;
    assert!(denied.is_error, "missing connection authority must deny");
    assert!(
        denied.content.contains("denied by policy"),
        "{}",
        denied.content
    );
    assert_eq!(
        denied_backend.list_calls(),
        0,
        "authorization denial must happen before backend IO"
    );
}
