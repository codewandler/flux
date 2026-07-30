//! C-290 — a host that constructs a runtime can bound what it *uses*, not just what it *spends*.
//!
//! Two ceilings are covered here, both enforced inside the safety envelope (`Executor::dispatch`),
//! so they bind for **in-process embedding** and not only for `flux-server`:
//!
//! * a **concurrency limit** — how many tool calls may be executing simultaneously;
//! * a **retained-result byte ceiling** — how much of the runtime's own result retention (the
//!   deterministic op cache) it may hold at once.
//!
//! Every concurrency assertion here uses an op that **blocks until released**, so "in flight" means
//! "inside `Tool::execute`", not "recently returned".

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use flux_core::Result;
use flux_provider::{ChunkStream, Provider, Request};
use flux_runtime::{
    AllowApprover, ExecutionAuthorization, ExecutionEnvironment, Executor, PermissionManager,
    ResourceLimits, Tool, ToolContext, ToolRegistry, ToolResult,
};
use flux_sdk::dsl::*;
use flux_sdk::{Client, FlowClient};
use flux_spec::ToolSpec;
use flux_system::{System, Workspace};
use serde_json::{json, Value};
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

// ---------------------------------------------------------------------------
// Harness: an op that blocks until released, metering how many run at once.
// ---------------------------------------------------------------------------

/// Live and peak occupancy of `Tool::execute`, sampled by [`Blocker`] itself.
#[derive(Default)]
struct Meter {
    in_flight: AtomicUsize,
    peak: AtomicUsize,
}

impl Meter {
    fn enter(&self) {
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
    }

    fn leave(&self) {
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
    }

    fn in_flight(&self) -> usize {
        self.in_flight.load(Ordering::SeqCst)
    }

    fn peak(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }
}

/// A read-only op that parks inside `execute` until the test hands out a release permit.
struct Blocker {
    name: &'static str,
    meter: Arc<Meter>,
    release: Arc<Semaphore>,
}

#[async_trait]
impl Tool for Blocker {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            self.name,
            "blocks until the test releases it",
            json!({ "type": "object", "properties": {} }),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, _params: Value) -> Result<ToolResult> {
        self.meter.enter();
        // `forget` so the permit is consumed: each `add_permits` releases exactly one execution.
        self.release
            .acquire()
            .await
            .expect("release gate closed")
            .forget();
        self.meter.leave();
        Ok(ToolResult::ok("released"))
    }
}

/// A read-only op returning a fixed-size body, for the retained-bytes ceiling.
struct Bulk {
    bytes: usize,
}

#[async_trait]
impl Tool for Bulk {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "bulk",
            "returns a fixed-size body",
            json!({
                "type": "object",
                "properties": { "n": { "type": "integer" } },
                "required": ["n"],
            }),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, _params: Value) -> Result<ToolResult> {
        Ok(ToolResult::ok("x".repeat(self.bytes)))
    }
}

struct StubProvider;

#[async_trait]
impl Provider for StubProvider {
    fn name(&self) -> &str {
        "unused"
    }
    async fn stream(&self, _req: Request) -> Result<ChunkStream> {
        Ok(Box::pin(futures::stream::empty()))
    }
}

/// A unique temp workspace root for one test.
fn temp_root(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "flux-c290-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

/// A guarded environment over a temp workspace, carrying `limits` and the given ops.
fn environment(tag: &str, limits: ResourceLimits, ops: Vec<Arc<dyn Tool>>) -> ExecutionEnvironment {
    let system = Arc::new(System::new(Workspace::new(temp_root(tag)).unwrap()));
    let mut registry = ToolRegistry::new();
    // Pre-allow each op by name: a call the rules only "ask" for is approval-sensitive, and an
    // approval-sensitive call never enters the op cache — which the retained-bytes test needs.
    let mut allow = Vec::new();
    for op in ops {
        allow.push(op.spec().name);
        registry.register(op);
    }
    ExecutionEnvironment::new(
        system,
        registry,
        PermissionManager::from_rules(&allow, &[]),
        Arc::new(AllowApprover),
        ExecutionAuthorization::local(),
    )
    .with_resource_limits(limits)
}

/// Fire `n` concurrent dispatches of `op` and hand back their join handles.
fn fire(executor: &Arc<Executor>, op: &'static str, n: usize) -> Vec<JoinHandle<ToolResult>> {
    (0..n)
        .map(|_| {
            let executor = executor.clone();
            tokio::spawn(async move { executor.dispatch(op, json!({})).await })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The concurrency ceiling
// ---------------------------------------------------------------------------

/// **The C-290 acceptance test.** A runtime configured with a concurrency limit of N never has more
/// than N tool executions in flight — demonstrated with an op that blocks until released.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_concurrency_limit_caps_simultaneous_tool_executions() {
    let meter = Arc::new(Meter::default());
    let release = Arc::new(Semaphore::new(0));
    let executor = Arc::new(
        environment(
            "cap",
            ResourceLimits::new().with_max_concurrent_tool_calls(2),
            vec![Arc::new(Blocker {
                name: "blocker",
                meter: meter.clone(),
                release: release.clone(),
            })],
        )
        .into_executor(),
    );

    let handles = fire(&executor, "blocker", 6);
    // Long enough for all six to have reached the envelope; only two may be inside `execute`.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        meter.in_flight(),
        2,
        "the concurrency limit of 2 did not bind: {} executions were in flight",
        meter.in_flight()
    );

    // Release them one at a time; the ceiling must hold for the whole drain, not just at the start.
    for _ in 0..6 {
        release.add_permits(1);
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(
            meter.in_flight() <= 2,
            "in-flight rose to {} above the limit of 2 while draining",
            meter.in_flight()
        );
    }
    for handle in handles {
        let result = handle.await.unwrap();
        assert!(!result.is_error, "dispatch failed: {}", result.content);
    }
    assert_eq!(meter.peak(), 2, "peak occupancy must equal the limit");
}

/// The ceiling is a property of the configured runtime, not of one executor instance: every
/// executor derived from the same environment shares it. That is what makes it a *host* ceiling —
/// a surface that mints a fresh executor per run (`FlowClient::build_executor`) cannot escape it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_ceiling_is_shared_by_every_executor_derived_from_one_environment() {
    let meter = Arc::new(Meter::default());
    let release = Arc::new(Semaphore::new(0));
    let environment = environment(
        "shared",
        ResourceLimits::new().with_max_concurrent_tool_calls(1),
        vec![Arc::new(Blocker {
            name: "blocker",
            meter: meter.clone(),
            release: release.clone(),
        })],
    );
    let first = Arc::new(environment.clone().into_executor());
    let second = Arc::new(environment.into_executor());

    let mut handles = fire(&first, "blocker", 2);
    handles.extend(fire(&second, "blocker", 2));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        meter.in_flight(),
        1,
        "two executors from one environment ran {} tools at once under a limit of 1",
        meter.in_flight()
    );
    release.add_permits(4);
    for handle in handles {
        handle.await.unwrap();
    }
    assert_eq!(meter.peak(), 1);
}

/// Exceeding the limit is an observable, actionable refusal — never a hang. A call that cannot get
/// a slot within the configured queue timeout comes back as a `ToolResult` error that names the
/// limit and the knob to raise, and it comes back *promptly*.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_saturated_runtime_refuses_instead_of_hanging() {
    let meter = Arc::new(Meter::default());
    let release = Arc::new(Semaphore::new(0));
    let executor = Arc::new(
        environment(
            "saturated",
            ResourceLimits::new()
                .with_max_concurrent_tool_calls(1)
                .with_tool_call_queue_timeout(Duration::from_millis(100)),
            vec![Arc::new(Blocker {
                name: "blocker",
                meter: meter.clone(),
                release: release.clone(),
            })],
        )
        .into_executor(),
    );

    let held = fire(&executor, "blocker", 1);
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(meter.in_flight(), 1);

    let started = std::time::Instant::now();
    let refused = executor.dispatch_outcome("blocker", json!({})).await;
    let waited = started.elapsed();

    assert!(
        waited < Duration::from_secs(5),
        "the refusal took {waited:?} — a saturated runtime must not hang"
    );
    assert!(refused.result.is_error, "a saturated runtime must refuse");
    let message = &refused.result.content;
    assert!(
        message.contains("concurrency limit") && message.contains('1'),
        "the refusal must name the limit that bound it, got: {message}"
    );
    assert!(
        message.contains("max_concurrent_tool_calls"),
        "the refusal must name the knob to raise, got: {message}"
    );
    assert!(
        !refused.denied,
        "a resource refusal is transient, not an authorization denial"
    );
    // Observable in the audit trail, not just in the returned string.
    assert!(
        executor
            .evidence()
            .all()
            .iter()
            .any(|o| o.kind == "tool_concurrency_refused"),
        "the refusal must be recorded as an observation"
    );

    release.add_permits(2);
    for handle in held {
        handle.await.unwrap();
    }
}

/// Unset (the default) means no ceiling — configuring nothing must not start bounding anything.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_configured_limit_means_no_ceiling() {
    let meter = Arc::new(Meter::default());
    let release = Arc::new(Semaphore::new(0));
    let executor = Arc::new(
        environment(
            "unset",
            ResourceLimits::new(),
            vec![Arc::new(Blocker {
                name: "blocker",
                meter: meter.clone(),
                release: release.clone(),
            })],
        )
        .into_executor(),
    );
    let handles = fire(&executor, "blocker", 5);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(meter.in_flight(), 5, "an unset limit must not bind");
    release.add_permits(5);
    for handle in handles {
        handle.await.unwrap();
    }
}

// ---------------------------------------------------------------------------
// Reachable from the builders
// ---------------------------------------------------------------------------

/// The ceiling binds through a builder-constructed `FlowClient` running a real authored `parallel`
/// flow — the in-process shape that actually produces concurrent tool executions.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_flow_client_parallel_block_is_bounded_by_the_configured_ceiling() {
    let meter = Arc::new(Meter::default());
    let release = Arc::new(Semaphore::new(0));
    let mut client = FlowClient::builder()
        .model("mock")
        .auto_approve(true)
        .resource_limits(ResourceLimits::new().with_max_concurrent_tool_calls(1))
        .build(Arc::new(StubProvider), temp_root("flow"))
        .expect("build FlowClient");
    for name in ["one", "two", "three"] {
        client.register_op(Arc::new(Blocker {
            name,
            meter: meter.clone(),
            release: release.clone(),
        }));
    }

    let flow = Flow::named("bounded")
        .body(|b| {
            b.parallel(|p| {
                p.branch("a", |bb| {
                    bb.call("one", [lit("x")]);
                });
                p.branch("b", |bb| {
                    bb.call("two", [lit("x")]);
                });
                p.branch("c", |bb| {
                    bb.call("three", [lit("x")]);
                });
            });
        })
        .build();
    if let Err(diags) = client.analyze(&flow) {
        panic!("analyze failed: {diags:?}");
    }

    // Feed the release gate from the side: with a ceiling of 1 the flow can only ever be holding
    // one op, so a permit per branch drains it in lockstep.
    let releaser = {
        let release = release.clone();
        let meter = meter.clone();
        tokio::spawn(async move {
            for _ in 0..3 {
                tokio::time::sleep(Duration::from_millis(120)).await;
                assert!(
                    meter.in_flight() <= 1,
                    "a parallel flow ran {} ops at once under a limit of 1",
                    meter.in_flight()
                );
                release.add_permits(1);
            }
        })
    };
    client.execute(&flow).await.expect("execute");
    releaser.await.unwrap();
    assert_eq!(meter.peak(), 1, "peak occupancy must equal the limit");
}

/// The knob sits on `ClientBuilder` alongside `context_budget` / `max_iterations` / `max_tokens`,
/// and the built client reports what it was configured with.
#[test]
fn a_client_builder_carries_the_configured_limits() {
    let dir = temp_root("client");
    let client: Client = Client::builder()
        .model("mock")
        .context_budget(4096)
        .max_iterations(7)
        .max_tokens(256)
        .resource_limits(
            ResourceLimits::new()
                .with_max_concurrent_tool_calls(3)
                .with_max_retained_result_bytes(1024),
        )
        .build(Box::new(StubProvider), &dir)
        .expect("build Client");
    let limits = client.resource_limits();
    assert_eq!(limits.max_concurrent_tool_calls(), Some(3));
    assert_eq!(limits.max_retained_result_bytes(), Some(1024));
    std::fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// The retained-result byte ceiling (the narrowed memory half)
// ---------------------------------------------------------------------------

/// The runtime's own result retention is bounded in **bytes**, not just entries: caching a result
/// that would push the retained total past the ceiling evicts instead of growing. A miss is
/// correctness-neutral (the op re-runs), so this bound never truncates anything model-visible.
#[tokio::test]
async fn the_retained_result_ceiling_bounds_what_the_executor_keeps() {
    let executor = environment(
        "retained",
        ResourceLimits::new().with_max_retained_result_bytes(4_096),
        vec![Arc::new(Bulk { bytes: 1_000 })],
    )
    .into_executor()
    .with_op_cache(true);

    for n in 0..20 {
        let result = executor.dispatch("bulk", json!({ "n": n })).await;
        assert!(!result.is_error, "{}", result.content);
    }
    assert!(
        executor.retained_result_bytes() <= 4_096,
        "retained {} bytes, above the 4096-byte ceiling",
        executor.retained_result_bytes()
    );
    assert!(
        executor.retained_result_bytes() > 0,
        "the cache should still be retaining something under the ceiling"
    );
}
