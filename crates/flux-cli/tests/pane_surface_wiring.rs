//! C-305: the `pane.*` vocabulary, wired to a real surface.
//!
//! C-223 landed the ops and the fail-closed surfacing seam
//! (`flux_tools::try_register_surface_ops`) but nothing called it, so no model ever saw a `pane.*`
//! op and no pane command could reach a terminal. This suite pins the wiring `run_tui` performs, in
//! **both** directions, and it deliberately asserts on the *delivered command* rather than on the
//! sink object being installed — the first symptom of getting this wrong is silence, not a crash.
//!
//! Two things make the assertions here worth their length:
//!
//! - **The engine's per-turn context is authoritative, including its absent fields.** Every turn
//!   runs inside `scope_runtime_turn(RuntimeTurnContext…)`, and `ToolContext::runtime_turn_context`
//!   reads the executor's stored fallback *only outside* such a scope. A sink installed on the
//!   executor alone therefore looks correct and delivers nothing — `ctx.surface()` returns `None`
//!   for every op in every real turn. The end-to-end test below runs an actual turn for exactly
//!   that reason; a test that dispatched `pane.open` outside a turn would pass against the broken
//!   wiring.
//! - **A registered op with no `group` is advertised unconditionally.** So the headless direction
//!   cannot be checked by looking at surfacing: it has to be checked at registration, which is why
//!   `try_register_surface_ops` exists at all.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use flux_core::{Chunk, ContentBlock, StopReason};
use flux_events::EventStore;
use flux_flow::agent_sink::AgentSink;
use flux_flow::engine::FlowEngine;
use flux_flow::state::FlowStore;
use flux_provider::{ChunkStream, Provider, Request};
use flux_runtime::{
    AllowApprover, Executor, PaneCommand, PaneData, PaneLifetime, PaneSlot, PaneSpec,
    PermissionManager, SurfaceSink, ToolContext, ToolRegistry,
};
use flux_system::{System, Workspace};

static SCRATCH: AtomicU64 = AtomicU64::new(0);

fn scratch_dir(label: &str) -> std::path::PathBuf {
    let n = SCRATCH.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("flux-c305-{label}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create the test workspace root");
    dir
}

/// A model that opens one pane and then finishes.
///
/// Request-aware rather than a fixed script, for the same reason `flux-cli`'s own mock provider is:
/// the *provider-native* name of a dotted operation is not its Flux name (`native_tool_name` maps
/// `pane.open` to a portable `pane_open__<digest>`), so the call has to be built from the tool set
/// the stage actually advertised. Hard-coding `"pane.open"` would be rejected as an unknown
/// operation and the test would pass or fail for reasons that have nothing to do with the wiring.
#[derive(Default)]
struct PaneOpeningModel {
    /// Every operation name this model was offered, per stage — read by the catalog assertions so
    /// "the op is advertised" and "the op was called" are separate observations.
    offered: Mutex<Vec<String>>,
    /// Planning rounds served since intent routing.
    rounds: AtomicUsize,
}

impl PaneOpeningModel {
    /// The advertised tool whose name is `pane.open` or its portable alias.
    fn pane_open_tool(req: &Request) -> Option<String> {
        req.tools
            .iter()
            .find(|tool| {
                tool.name == "pane.open"
                    || tool.name.starts_with("pane_open")
                    || tool.description.contains("`pane.open`")
            })
            .map(|tool| tool.name.clone())
    }
}

#[async_trait::async_trait]
impl Provider for PaneOpeningModel {
    fn name(&self) -> &str {
        "scripted"
    }

    async fn stream(&self, req: Request) -> flux_core::Result<ChunkStream> {
        self.offered
            .lock()
            .unwrap()
            .extend(req.tools.iter().map(|tool| tool.name.clone()));

        // Intent routing sees only the family index. `core` is where `virtual_family` files an op
        // with no effects and no access — which is exactly what a pane is.
        if req.tools.len() == 1 && req.tools[0].name == "declare_intent" {
            return Ok(stream_of(native_call(
                "intent-1",
                "declare_intent",
                json!({
                    "intent": "show the build on a pane",
                    "capability_families": ["core"],
                }),
            )));
        }

        // One planning round proposes the pane, the next freezes the batch, the last narrates. A
        // counter rather than a transcript probe: this model must terminate even if a stage repairs,
        // or a wiring bug shows up as a budget-exhaustion error instead of as the missing pane.
        let round = self.rounds.fetch_add(1, Ordering::SeqCst);
        let offers_finalize = req.tools.iter().any(|tool| tool.name == "finalize_plan");
        let chunks = match (round, Self::pane_open_tool(&req)) {
            (0, Some(native)) => native_call(
                PANE_CALL_ID,
                &native,
                json!({
                    "id": PANE_ID,
                    "title": PANE_TITLE,
                    "data": { "log": { "lines": ["compiling flux-cli"] } }
                }),
            ),
            (1, _) if offers_finalize => native_call(
                "finalize-1",
                "finalize_plan",
                json!({ "instructions": "Say the pane is up." }),
            ),
            _ => prose("The build pane is up."),
        };
        Ok(stream_of(chunks))
    }
}

fn stream_of(chunks: Vec<Chunk>) -> ChunkStream {
    Box::pin(futures::stream::iter(chunks.into_iter().map(Ok)))
}

fn native_call(id: &str, name: &str, input: Value) -> Vec<Chunk> {
    vec![
        Chunk::Block(ContentBlock::ToolUse {
            id: id.into(),
            name: name.into(),
            input,
        }),
        Chunk::Done {
            stop_reason: Some(StopReason::ToolUse),
        },
    ]
}

fn prose(text: &str) -> Vec<Chunk> {
    vec![
        Chunk::TextDelta(text.into()),
        Chunk::Done {
            stop_reason: Some(StopReason::EndTurn),
        },
    ]
}

/// The `pane.open` call the scripted model makes: a `log` pane in the default slot/lifetime.
const PANE_ID: &str = "build";
const PANE_TITLE: &str = "Build progress";
const PANE_CALL_ID: &str = "pane-call-1";

/// Assemble an engine the way `build_agent_with` does for a given surface: the pane ops are
/// registered **iff** a sink was minted, and the same sink is installed on the engine so it reaches
/// every turn. Passing `None` is the headless posture of `flux run`, `flux-server` and the SDK.
fn assemble_for_surface(
    label: &str,
    surface_sink: Option<Arc<dyn flux_runtime::SurfaceSink>>,
    provider: Arc<dyn Provider>,
) -> (FlowEngine, Arc<EventStore>) {
    let dir = scratch_dir(label);
    let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
    let mut registry = ToolRegistry::new();
    flux_tools::register_reflect(&mut registry);
    flux_tools::register_evidence(&mut registry);
    // The one seam under test: fail-closed, decided once, at assembly time.
    flux_tools::try_register_surface_ops(&mut registry, surface_sink.is_some())
        .expect("the pane pack registers");
    let executor = Executor::new(
        registry,
        PermissionManager::from_rules(
            &flux_tools::PANE_OPS
                .iter()
                .map(|op| (*op).to_string())
                .collect::<Vec<_>>(),
            &[],
        ),
        Arc::new(AllowApprover),
        ToolContext::new(system),
    );
    let events = Arc::new(EventStore::in_memory().unwrap());
    let flow = FlowStore::in_memory_with_events(events.clone()).unwrap();
    let mut engine = FlowEngine::assemble(
        provider,
        executor,
        events.clone(),
        flow,
        "scripted/test-model".into(),
        "Use only observed evidence.".into(),
        2_048,
        8,
        Vec::new(),
        0,
        Vec::new(),
        dir,
    )
    .expect("the engine assembles");
    if let Some(sink) = surface_sink {
        engine = engine.with_surface_sink(sink);
    }
    (engine, events)
}

#[derive(Default)]
struct SilentSink;
impl AgentSink for SilentSink {}

/// **Acceptance 5 — the one that matters.** A `pane.open` the *model* calls, dispatched through the
/// real envelope inside a real turn, ends up drawn on a terminal.
///
/// Every link is the production one, and after the C-305 rework that now includes the last two:
/// `try_register_surface_ops` puts the op in the catalog, the engine carries the minted sink into
/// the turn's `RuntimeTurnContext`, `ToolContext::surface` hands the op a redacting
/// `SurfaceReporter`, `flux_tui::session_state` attaches the queue to the state the loop draws
/// from, and the loop itself drains it. This test used to *reconstruct* those last two by building
/// its own `ChatState::for_session(..).with_pane_queue(..)` and calling `apply_pending_panes()`
/// — which meant deleting both production lines left it green while no pane could ever reach a
/// terminal. It now calls them.
///
/// Before C-305 this failed at the *second* link: the op was never registered, and even with it
/// registered the turn context carried no sink, so `pane.open` would have failed with "no surface
/// is attached".
#[tokio::test]
async fn a_model_pane_open_reaches_the_tui_pane_state() {
    let panes = flux_tui::PaneQueue::new();
    let model = Arc::new(PaneOpeningModel::default());
    let (engine, events) = assemble_for_surface(
        "tui",
        Some(panes.clone() as Arc<dyn SurfaceSink>),
        model.clone(),
    );

    let session = events.create_session("scripted/test-model").unwrap();
    let mut sink = SilentSink;
    engine
        .run_turn(&session, "Show me the build", &mut sink)
        .await
        .expect("the scripted turn completes");

    // Advertisement and delivery are separate claims, so they get separate assertions: this one
    // says the planner was actually *offered* the op, which is what `try_register_surface_ops`
    // buys. It would still hold if nothing were ever delivered.
    let offered = model.offered.lock().unwrap().clone();
    assert!(
        offered.iter().any(|name| name.starts_with("pane_open")),
        "the planner was never offered `pane.open`; it saw {offered:?}"
    );

    // The surface side, through the production path: the options carry the handle minted before the
    // agent, `session_state` installs it on the state the loop draws from, and the loop drains it.
    let mut options = flux_tui::TuiRunOptions::new(false, None);
    options.pane_queue = Some(panes);
    let mut state = flux_tui::session_state(&engine, &session, &options)
        .expect("the TUI assembles its session state");
    let screen = flux_tui::drive_event_loop_headless(engine, &mut state, Vec::new())
        .await
        .expect("the headless loop draws one frame and exits on end-of-input");

    let open = state.open_panes();
    assert_eq!(
        open.len(),
        1,
        "the model's `pane.open` never reached the surface — the vocabulary is still inert; got \
         {open:?}"
    );
    assert_eq!(open[0].id, PANE_ID);
    assert_eq!(open[0].title, PANE_TITLE);
    assert!(
        !open[0].host_owned,
        "a model-authored pane must never be labelled host-owned — that mark is the trust chrome"
    );
    assert!(
        screen.contains(PANE_TITLE),
        "the pane never made it onto a frame:\n{screen}"
    );
}

/// **The install, on its own.** The state the TUI assembles for a session carries the very pane
/// channel the caller minted before the agent.
///
/// Deliberately blind to the *drain*: it asserts on what `flux_tui::session_state` returns, so
/// deleting `state.apply_pending_panes()` from the event loop leaves it green and only deleting the
/// `options.pane_queue` → `with_pane_queue` install reds it. That is what makes the pair above and
/// below independent proofs rather than one proof counted twice.
///
/// The production function is *called*, not reimagined: a test that built its own
/// `ChatState::for_session(..).with_pane_queue(..)` would pass with the install gone, which is
/// exactly how this link stayed unpinned through the first round.
#[tokio::test]
async fn the_state_the_tui_assembles_carries_the_pane_channel_the_agent_writes_to() {
    let panes = flux_tui::PaneQueue::new();
    let (engine, events) = assemble_for_surface(
        "session-state",
        Some(panes.clone() as Arc<dyn SurfaceSink>),
        Arc::new(PaneOpeningModel::default()),
    );
    let session = events.create_session("scripted/test-model").unwrap();

    let mut options = flux_tui::TuiRunOptions::new(false, None);
    options.pane_queue = Some(panes.clone());
    // Emitted exactly as a `pane.*` op's `SurfaceReporter` emits it, on the agent's half of the
    // channel.
    (panes as Arc<dyn SurfaceSink>).emit(PaneCommand::Open(open_spec()));

    let mut state = flux_tui::session_state(&engine, &session, &options)
        .expect("the TUI assembles its session state");
    assert_eq!(
        state.apply_pending_panes(),
        1,
        "the assembled state drains a different channel than the agent writes to — the `pane.*` \
         ops enqueue into a queue nobody reads and the surface stays silent"
    );
    assert_eq!(
        state
            .open_panes()
            .iter()
            .map(|p| p.id.as_str())
            .collect::<Vec<_>>(),
        vec![PANE_ID]
    );
}

/// **The drain, on its own.** The event loop applies the agent's queued pane commands into the
/// state it draws, every frame.
///
/// Deliberately blind to the *install*: the queue is attached to the state by the test, so deleting
/// `session_state`'s install leaves this green and only deleting `state.apply_pending_panes()` at
/// the top of the loop reds it. Before this test existed, deleting that line left the whole
/// flux-tui + flux-cli surface green while no model pane could ever reach a terminal — the "first
/// symptom is silence" failure this story is named for.
///
/// The loop is the production one. It touches the concrete terminal in exactly one place
/// (`terminal.draw`) and the crossterm event source in exactly one (`input.next()`), which is why
/// both are parameters; `flux_tui::drive_event_loop_headless` supplies a `TestBackend` and an empty
/// event script, so the loop runs one frame and then takes its own end-of-input exit.
#[tokio::test]
async fn the_event_loop_drains_the_agents_pane_channel_into_the_frame_it_draws() {
    let panes = flux_tui::PaneQueue::new();
    let (engine, events) = assemble_for_surface(
        "event-loop",
        Some(panes.clone() as Arc<dyn SurfaceSink>),
        Arc::new(PaneOpeningModel::default()),
    );
    let session = events.create_session("scripted/test-model").unwrap();

    // Queued before the loop starts, exactly as a `pane.open` executed inside a turn leaves it.
    (panes.clone() as Arc<dyn SurfaceSink>).emit(PaneCommand::Open(open_spec()));
    let mut state = flux_tui::ChatState::for_session("scripted/test-model".into(), session)
        .with_pane_queue(panes);

    let screen = flux_tui::drive_event_loop_headless(engine, &mut state, Vec::new())
        .await
        .expect("the headless loop draws one frame and exits on end-of-input");

    assert_eq!(
        state.open_panes().len(),
        1,
        "the event loop never drained the pane channel — a model pane cannot reach a terminal"
    );
    assert!(
        screen.contains(PANE_TITLE),
        "the drained pane was never drawn:\n{screen}"
    );
}

/// The `pane.open` the scripted model makes, as a already-decoded command — same id/title/payload,
/// so the two single-link tests assert against the same pane the end-to-end one does.
fn open_spec() -> PaneSpec {
    PaneSpec::new(
        PANE_ID,
        PANE_TITLE,
        PaneSlot::Right,
        PaneLifetime::Session,
        PaneData::Log {
            lines: vec!["compiling flux-cli".into()],
        },
    )
}

/// **Acceptance 3 — the fail-closed half.** A headless assembly advertises no `pane.*` op at all,
/// and a call to one is refused rather than silently dropped.
///
/// This is checked at *registration* on purpose: a registered op with no `group` is advertised
/// unconditionally (`ToolRegistry::active_specs` filters on groups alone), so there is no later
/// gate that could hold these back from a `flux run`, `flux-server` or SDK catalog.
#[tokio::test]
async fn a_headless_assembly_never_advertises_or_dispatches_a_pane_op() {
    let (engine, _events) =
        assemble_for_surface("headless", None, Arc::new(PaneOpeningModel::default()));

    for op in flux_tools::PANE_OPS {
        assert!(
            engine.executor.registry().get(op).is_none(),
            "`{op}` is in a headless catalog — a session with no surface must never see it"
        );
    }
    let refused = engine
        .executor
        .dispatch(
            "pane.open",
            json!({ "id": "x", "title": "x", "data": {"log": {"lines": []}} }),
        )
        .await;
    assert!(
        refused.is_error,
        "a headless `pane.open` must fail, not succeed into a void: {}",
        refused.content
    );
}

/// **Acceptance 4.** The surfacing decision is taken once, at assembly. Dropping every surface-side
/// handle mid-session must not move the advertised tool set — a churning catalog would invalidate
/// the provider prompt prefix (the A-95 lesson C-223's own no-churn test pins at the registry).
#[tokio::test]
async fn the_advertised_catalog_does_not_churn_when_the_surface_handle_goes_away() {
    let panes = flux_tui::PaneQueue::new();
    let (engine, _events) = assemble_for_surface(
        "no-churn",
        Some(panes.clone() as Arc<dyn flux_runtime::SurfaceSink>),
        Arc::new(PaneOpeningModel::default()),
    );
    let before = engine.executor.registry().names();
    drop(panes);
    let after = engine.executor.registry().names();
    assert_eq!(
        before, after,
        "the catalog changed after the surface handle was dropped — surfacing is no longer an \
         assembly-time decision"
    );
    for op in flux_tools::PANE_OPS {
        assert!(
            after.iter().any(|name| name == op),
            "`{op}` left the catalog"
        );
    }
}

/// **Safety invariant (AGENTS.md).** Installing a sink must not make the pane ops' declared
/// authority a lie: an op declaring a `Write` effect while reporting no subjects is forced to
/// approval, and an under-declared one would slip a gate. Verified with
/// `flux_spec::metadata_violations` over the *assembled TUI catalog* rather than by eye.
#[test]
fn the_tui_pane_catalog_declares_honest_metadata_and_names_its_subjects() {
    let mut registry = ToolRegistry::new();
    flux_tools::try_register_surface_ops(&mut registry, true).expect("the pane pack registers");

    for op in flux_tools::PANE_OPS {
        let tool = registry.get(op).expect("the pane op is registered");
        let violations = flux_spec::metadata_violations(&tool.spec(), &tool.semantic_effects());
        assert!(
            violations.is_empty(),
            "`{op}` declares incoherent metadata: {violations:?}"
        );
        let subjects = tool.permission_subjects(&json!({ "id": PANE_ID }));
        assert_eq!(
            subjects,
            vec![PANE_ID.to_string()],
            "`{op}` must name the pane it addresses so a policy can scope it"
        );
    }
}
