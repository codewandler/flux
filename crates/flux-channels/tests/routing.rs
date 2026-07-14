//! Routing: a delivered event runs the matching trigger's journey; the App serializes deliveries.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex, Weak,
};
use std::time::Duration;

use async_trait::async_trait;
use flux_app::{App, Bus};
use flux_channels::{AppDeliverer, Deliverer};
use flux_lang::program::Module;
use flux_runtime::{Tool, ToolContext, ToolResult};
use flux_spec::ToolSpec;
use serde_json::json;
use tokio::sync::Notify;

/// A tiny program: a `tick` trigger → a pure-op journey that returns the literal `"ok"`. No provider.
fn tick_app() -> Arc<App> {
    let src = "\
trigger t
  on \"tick\"
  run tick

journey tick
  flow
    return \"ok\"
";
    let program = match Module::parse_str(src).unwrap() {
        Module::Program(p) => p,
        Module::Flow(_) => unreachable!("a program"),
    };
    Arc::new(App::with_options(program, None, "mock", true))
}

/// Holds an initial journey briefly so a second delivery can enter the same vulnerable broadcast
/// subscription window. Once App-owned serialization exists, the first call simply times out and
/// proceeds; this keeps the regression test from encoding concurrency as a requirement.
struct OverlapWindow {
    entrants: AtomicUsize,
    peer_arrived: Notify,
}

/// Holds one delivery inside its first journey so the test can inject an unrelated public bus event
/// while the App's delivery coordinator owns the root.
struct DeliveryBarrier {
    entered: Notify,
    release: Notify,
}

struct ForeignEmitter {
    bus: Bus,
}

struct ReentrantDeliver {
    app: Mutex<Option<Weak<App>>>,
}

impl ReentrantDeliver {
    fn new() -> Self {
        Self {
            app: Mutex::new(None),
        }
    }

    fn bind(&self, app: &Arc<App>) {
        *self.app.lock().expect("reentrant app lock poisoned") = Some(Arc::downgrade(app));
    }
}

#[async_trait]
impl Tool for ReentrantDeliver {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "reentrant_deliver",
            "attempt to re-enter the current App delivery",
            json!({"type": "object"}),
        )
    }

    async fn execute(
        &self,
        _ctx: &ToolContext,
        _params: serde_json::Value,
    ) -> flux_core::Result<ToolResult> {
        let app = self
            .app
            .lock()
            .expect("reentrant app lock poisoned")
            .as_ref()
            .and_then(Weak::upgrade)
            .expect("reentrant App is bound");
        let message = match app.deliver("nested", json!({})).await {
            Ok(_) => "unexpectedly re-entered App delivery".to_string(),
            Err(error) => error.to_string(),
        };
        Ok(ToolResult::ok(message))
    }
}

#[async_trait]
impl Tool for ForeignEmitter {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "foreign_emit",
            "emit on a different App's bus",
            json!({"type": "object"}),
        )
    }

    async fn execute(
        &self,
        _ctx: &ToolContext,
        _params: serde_json::Value,
    ) -> flux_core::Result<ToolResult> {
        let accepted = self.bus.emit("foreign", json!({}));
        Ok(ToolResult::ok(accepted.to_string()))
    }
}

impl DeliveryBarrier {
    fn new() -> Self {
        Self {
            entered: Notify::new(),
            release: Notify::new(),
        }
    }
}

#[async_trait]
impl Tool for DeliveryBarrier {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "delivery_barrier",
            "hold a delivery open for supervisor ownership tests",
            json!({"type": "object"}),
        )
    }

    async fn execute(
        &self,
        _ctx: &ToolContext,
        _params: serde_json::Value,
    ) -> flux_core::Result<ToolResult> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(ToolResult::ok("released"))
    }
}

impl OverlapWindow {
    fn new() -> Self {
        Self {
            entrants: AtomicUsize::new(0),
            peer_arrived: Notify::new(),
        }
    }
}

#[async_trait]
impl Tool for OverlapWindow {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "overlap_window",
            "coordinate concurrent delivery tests",
            json!({"type": "object"}),
        )
    }

    async fn execute(
        &self,
        _ctx: &ToolContext,
        _params: serde_json::Value,
    ) -> flux_core::Result<ToolResult> {
        let peer_arrived = self.peer_arrived.notified();
        if self.entrants.fetch_add(1, Ordering::SeqCst) + 1 >= 2 {
            self.peer_arrived.notify_waiters();
        } else if self.entrants.load(Ordering::SeqCst) < 2 {
            let _ = tokio::time::timeout(Duration::from_millis(100), peer_arrived).await;
        }
        Ok(ToolResult::ok("ready"))
    }
}

/// Both initial journeys emit onto the shared broadcast bus. Without one coordinator owned by the
/// App, overlapping delivery receivers each consume the other's `followup` event and run `cascade`
/// more than once.
fn cascading_app() -> Arc<App> {
    let src = r#"
channel cli

trigger initial_trigger
  on "initial"
  run initial

trigger cascade_trigger
  on "followup"
  run cascade

journey initial
  flow
    overlap_window({})
    emit({ "event": "followup", "payload": { "delivery": $delivery } })

journey cascade
  flow
    send({ "channel": "cli", "message": "cascade" })
    return $delivery
"#;
    let program = match Module::parse_str(src).unwrap() {
        Module::Program(p) => p,
        Module::Flow(_) => unreachable!("a program"),
    };
    Arc::new(App::with_tools(
        program,
        None,
        "mock",
        true,
        vec![Arc::new(OverlapWindow::new())],
    ))
}

fn startup_app() -> Arc<App> {
    let src = r#"
channel cli

trigger startup_trigger
  on "startup"
  run started

journey started
  flow
    send({ "channel": "cli", "message": "started" })
"#;
    let program = match Module::parse_str(src).unwrap() {
        Module::Program(p) => p,
        Module::Flow(_) => unreachable!("a program"),
    };
    Arc::new(App::with_options(program, None, "mock", true))
}

fn interleaved_app() -> (Arc<App>, Arc<DeliveryBarrier>) {
    let src = r#"
channel cli

trigger initial_trigger
  on "initial"
  run initial

trigger cascade_trigger
  on "followup"
  run cascade

trigger external_trigger
  on "external"
  run external

journey initial
  flow
    delivery_barrier({})
    emit({ "event": "followup", "payload": { "delivery": $delivery } })
    return $delivery

journey cascade
  flow
    send({ "channel": "cli", "message": "cascade" })
    return $delivery

journey external
  flow
    send({ "channel": "cli", "message": "external" })
    return $delivery
"#;
    let program = match Module::parse_str(src).unwrap() {
        Module::Program(p) => p,
        Module::Flow(_) => unreachable!("a program"),
    };
    let barrier = Arc::new(DeliveryBarrier::new());
    let app = Arc::new(App::with_tools(
        program,
        None,
        "mock",
        true,
        vec![barrier.clone()],
    ));
    (app, barrier)
}

async fn wait_for_supervisor(app: &App) {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if app.bus().emit("__routing_probe", json!(null)) > 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("App supervisor did not start");
}

async fn wait_for_sent(app: &App, expected: usize) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while app.bus().sent().len() < expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("journey did not send the expected message");
}

fn assert_one_cascade_per_delivery(runs: &[flux_app::JourneyRun], delivery: &str) {
    let journeys: Vec<&str> = runs.iter().map(|run| run.journey.as_str()).collect();
    assert_eq!(journeys, ["initial", "cascade"]);
    assert_eq!(runs[1].result, delivery, "cascade stays correlated");
}

#[tokio::test]
async fn delivered_event_runs_matching_journey() {
    let d = AppDeliverer::new(tick_app());
    let runs = d.deliver("tick", json!({})).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].journey, "tick");
    assert_eq!(runs[0].result.trim(), "ok");
}

#[tokio::test]
async fn unmatched_label_runs_nothing() {
    let d = AppDeliverer::new(tick_app());
    let runs = d.deliver("nope", json!({})).await.unwrap();
    assert!(runs.is_empty());
}

/// Concurrent deliveries are serialized by the gate (no panic / corruption / cross-talk): every caller
/// still gets exactly its own journey result.
#[tokio::test]
async fn concurrent_deliveries_are_serialized() {
    let d = Arc::new(AppDeliverer::new(tick_app()));
    let mut handles = Vec::new();
    for _ in 0..8 {
        let d = d.clone();
        handles.push(tokio::spawn(
            async move { d.deliver("tick", json!({})).await },
        ));
    }
    for h in handles {
        let runs = h.await.unwrap().unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].result.trim(), "ok");
    }
}

#[tokio::test]
async fn direct_concurrent_app_deliveries_do_not_cross_consume_cascades() {
    let app = cascading_app();

    let (left, right) = tokio::join!(
        app.deliver("initial", json!({"delivery": "left"})),
        app.deliver("initial", json!({"delivery": "right"})),
    );

    assert_one_cascade_per_delivery(&left.unwrap(), "left");
    assert_one_cascade_per_delivery(&right.unwrap(), "right");
    assert_eq!(app.bus().sent().len(), 2, "each cascade mutates once");
}

#[tokio::test]
async fn independent_app_deliverers_share_one_delivery_coordinator() {
    let app = cascading_app();
    let left = AppDeliverer::new(app.clone());
    let right = AppDeliverer::new(app.clone());

    let (left_runs, right_runs) = tokio::join!(
        left.deliver("initial", json!({"delivery": "left"})),
        right.deliver("initial", json!({"delivery": "right"})),
    );

    assert_one_cascade_per_delivery(&left_runs.unwrap(), "left");
    assert_one_cascade_per_delivery(&right_runs.unwrap(), "right");
    assert_eq!(app.bus().sent().len(), 2, "each cascade mutates once");
}

#[tokio::test]
async fn running_supervisor_and_direct_delivery_share_one_cascade_owner() {
    let app = cascading_app();
    let run = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });
    wait_for_supervisor(&app).await;

    let runs = app
        .deliver("initial", json!({"delivery": "direct"}))
        .await
        .unwrap();
    assert_one_cascade_per_delivery(&runs, "direct");
    wait_for_sent(&app, 1).await;
    tokio::task::yield_now().await;
    assert_eq!(app.bus().sent().len(), 1, "cascade executes exactly once");

    run.abort();
    let _ = run.await;
}

#[tokio::test]
async fn app_run_has_one_reusable_owner_and_emits_startup_once() {
    let app = startup_app();
    let first = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });
    wait_for_sent(&app, 1).await;

    let second = tokio::time::timeout(Duration::from_millis(200), app.run())
        .await
        .expect("a second App::run must fail promptly");
    assert!(
        second.is_err(),
        "a second App::run must not become a receiver"
    );
    assert_eq!(app.bus().sent().len(), 1, "startup executes only once");

    first.abort();
    let _ = first.await;

    let resumed = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });
    wait_for_supervisor(&app).await;
    assert!(
        !resumed.is_finished(),
        "the run lease is reusable after cancellation"
    );
    assert_eq!(
        app.bus().sent().len(),
        1,
        "startup remains a one-time event"
    );
    resumed.abort();
    let _ = resumed.await;
}

#[tokio::test]
async fn direct_bus_emit_interleaved_with_delivery_is_a_distinct_root() {
    let (app, barrier) = interleaved_app();
    let run = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });
    wait_for_supervisor(&app).await;

    let delivery = tokio::spawn({
        let app = app.clone();
        async move { app.deliver("initial", json!({"delivery": "direct"})).await }
    });
    tokio::time::timeout(Duration::from_secs(1), barrier.entered.notified())
        .await
        .expect("delivery did not reach the barrier");
    assert!(app.bus().emit("external", json!({"delivery": "outside"})) > 0);
    barrier.release.notify_one();

    let runs = delivery.await.unwrap().unwrap();
    assert_one_cascade_per_delivery(&runs, "direct");
    wait_for_sent(&app, 2).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let sent = app.bus().sent();
    assert_eq!(
        sent.iter()
            .filter(|message| message.message == "cascade")
            .count(),
        1,
        "the delivery cascade executes exactly once"
    );
    assert_eq!(
        sent.iter()
            .filter(|message| message.message == "external")
            .count(),
        1,
        "the unrelated public event executes exactly once"
    );

    run.abort();
    let _ = run.await;
}

#[tokio::test]
async fn one_apps_delivery_scope_cannot_tag_another_apps_bus() {
    let target_src = r#"
channel cli

trigger initial_trigger
  on "initial"
  run initial

trigger foreign_trigger
  on "foreign"
  run foreign

journey initial
  flow
    delivery_barrier({})
    return "target"

journey foreign
  flow
    send({ "channel": "cli", "message": "foreign" })
    return "foreign"
"#;
    let target_program = match Module::parse_str(target_src).unwrap() {
        Module::Program(program) => program,
        Module::Flow(_) => unreachable!("a program"),
    };
    let barrier = Arc::new(DeliveryBarrier::new());
    let target = Arc::new(App::with_tools(
        target_program,
        None,
        "mock",
        true,
        vec![barrier.clone()],
    ));

    let source_src = r#"
trigger initial_trigger
  on "initial"
  run initial

journey initial
  flow
    foreign_emit({})
    return "source"
"#;
    let source_program = match Module::parse_str(source_src).unwrap() {
        Module::Program(program) => program,
        Module::Flow(_) => unreachable!("a program"),
    };
    let source = Arc::new(App::with_tools(
        source_program,
        None,
        "mock",
        true,
        vec![Arc::new(ForeignEmitter {
            bus: target.bus().clone(),
        })],
    ));

    // Both actors are processing root 1. The source's task-local root must not become a causal
    // cascade in the target merely because the numeric counters match.
    let target_delivery = tokio::spawn({
        let target = target.clone();
        async move { target.deliver("initial", json!({})).await }
    });
    tokio::time::timeout(Duration::from_secs(1), barrier.entered.notified())
        .await
        .expect("target delivery did not reach the barrier");
    let source_runs = source.deliver("initial", json!({})).await.unwrap();
    assert_eq!(source_runs.len(), 1);
    barrier.release.notify_one();

    let target_runs = target_delivery.await.unwrap().unwrap();
    assert_eq!(
        target_runs
            .iter()
            .map(|run| run.journey.as_str())
            .collect::<Vec<_>>(),
        ["initial"]
    );
    assert!(
        target.bus().sent().is_empty(),
        "the foreign event requires the target App's own run lease"
    );
}

#[tokio::test]
async fn same_app_reentrant_delivery_fails_instead_of_deadlocking() {
    let src = r#"
trigger initial_trigger
  on "initial"
  run initial

trigger nested_trigger
  on "nested"
  run nested

journey initial
  flow
    $out = reentrant_deliver({})
    return $out

journey nested
  flow
    return "nested"
"#;
    let program = match Module::parse_str(src).unwrap() {
        Module::Program(program) => program,
        Module::Flow(_) => unreachable!("a program"),
    };
    let tool = Arc::new(ReentrantDeliver::new());
    let app = Arc::new(App::with_tools(
        program,
        None,
        "mock",
        true,
        vec![tool.clone()],
    ));
    tool.bind(&app);

    let runs = tokio::time::timeout(Duration::from_secs(1), app.deliver("initial", json!({})))
        .await
        .expect("same-App reentrant delivery deadlocked")
        .unwrap();
    assert_eq!(runs.len(), 1);
    assert!(
        runs[0].result.contains("cannot re-enter"),
        "unexpected reentrant result: {}",
        runs[0].result
    );
}

#[tokio::test]
async fn cancelling_blocked_delivery_releases_the_actor_for_the_next_request() {
    let (app, barrier) = interleaved_app();
    let blocked = tokio::spawn({
        let app = app.clone();
        async move { app.deliver("initial", json!({})).await }
    });
    tokio::time::timeout(Duration::from_secs(1), barrier.entered.notified())
        .await
        .expect("delivery did not reach the barrier");
    blocked.abort();
    let _ = blocked.await;

    let runs = tokio::time::timeout(
        Duration::from_secs(1),
        app.deliver("external", json!({"delivery": "after-cancel"})),
    )
    .await
    .expect("cancelled delivery left the actor blocked")
    .unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].journey, "external");
    assert_eq!(runs[0].result, "after-cancel");
}

#[tokio::test]
async fn cancelling_run_aborts_its_blocked_root_and_releases_the_run_lease() {
    let (app, barrier) = interleaved_app();
    let running = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });
    wait_for_supervisor(&app).await;
    assert!(app.bus().emit("initial", json!({})) > 0);
    tokio::time::timeout(Duration::from_secs(1), barrier.entered.notified())
        .await
        .expect("run-owned event did not reach the barrier");
    running.abort();
    let _ = running.await;

    let resumed = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });
    wait_for_supervisor(&app).await;
    let runs = tokio::time::timeout(
        Duration::from_secs(1),
        app.deliver("external", json!({"delivery": "after-run-cancel"})),
    )
    .await
    .expect("cancelled run left the actor blocked")
    .unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].result, "after-run-cancel");

    resumed.abort();
    let _ = resumed.await;
}

#[tokio::test]
async fn events_arriving_during_startup_queue_behind_it_and_run_once() {
    let src = r#"
channel cli

trigger startup_trigger
  on "startup"
  run startup

trigger external_trigger
  on "external"
  run external

journey startup
  flow
    delivery_barrier({})
    send({ "channel": "cli", "message": "startup-done" })

journey external
  flow
    send({ "channel": "cli", "message": "external" })
"#;
    let program = match Module::parse_str(src).unwrap() {
        Module::Program(program) => program,
        Module::Flow(_) => unreachable!("a program"),
    };
    let barrier = Arc::new(DeliveryBarrier::new());
    let app = Arc::new(App::with_tools(
        program,
        None,
        "mock",
        true,
        vec![barrier.clone()],
    ));
    let running = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });
    tokio::time::timeout(Duration::from_secs(1), barrier.entered.notified())
        .await
        .expect("startup did not reach the barrier");

    assert!(
        app.bus().emit("external", json!({})) > 0,
        "the run route is active once startup is durably queued"
    );
    assert!(
        app.bus().sent().is_empty(),
        "the external root must not overtake startup"
    );
    barrier.release.notify_one();
    wait_for_sent(&app, 2).await;
    assert_eq!(
        app.bus()
            .sent()
            .iter()
            .map(|message| message.message.as_str())
            .collect::<Vec<_>>(),
        ["startup-done", "external"]
    );

    running.abort();
    let _ = running.await;
}

#[tokio::test]
async fn observers_see_startup_before_events_it_cascades() {
    let src = r#"
trigger startup_trigger
  on "startup"
  run startup

journey startup
  flow
    emit({ "event": "followup" })
"#;
    let program = match Module::parse_str(src).unwrap() {
        Module::Program(program) => program,
        Module::Flow(_) => unreachable!("a program"),
    };
    let app = Arc::new(App::with_options(program, None, "mock", true));
    let mut observer = app.bus().subscribe();
    let running = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });

    let first = tokio::time::timeout(Duration::from_secs(1), observer.recv())
        .await
        .expect("startup observation timed out")
        .unwrap();
    let second = tokio::time::timeout(Duration::from_secs(1), observer.recv())
        .await
        .expect("cascade observation timed out")
        .unwrap();
    assert_eq!(
        [first.label.as_str(), second.label.as_str()],
        ["startup", "followup"]
    );

    running.abort();
    let _ = running.await;
}

#[tokio::test]
async fn queued_run_event_is_discarded_when_its_run_is_cancelled() {
    let (app, barrier) = interleaved_app();
    let running = tokio::spawn({
        let app = app.clone();
        async move { app.run().await }
    });
    wait_for_supervisor(&app).await;
    let direct = tokio::spawn({
        let app = app.clone();
        async move { app.deliver("initial", json!({"delivery": "direct"})).await }
    });
    tokio::time::timeout(Duration::from_secs(1), barrier.entered.notified())
        .await
        .expect("direct delivery did not reach the barrier");
    assert!(app.bus().emit("external", json!({})) > 0);
    running.abort();
    let _ = running.await;
    barrier.release.notify_one();

    let runs = direct.await.unwrap().unwrap();
    assert_one_cascade_per_delivery(&runs, "direct");
    tokio::task::yield_now().await;
    assert!(
        app.bus()
            .sent()
            .iter()
            .all(|message| message.message != "external"),
        "a queued root from the cancelled run must not execute"
    );
}
