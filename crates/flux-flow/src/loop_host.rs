//! Engine host for Flux-authored adaptive outer loops.
//!
//! The host exposes typed model stages and deterministic authored-flow execution to ordinary
//! Flux-Lang operations. Models use provider-native operation schemas; they never emit executable
//! Flux AST. Every gathered or approved action still runs through the shared [`Executor`].

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, Weak};

use async_trait::async_trait;
use serde_json::{json, Value};

use flux_core::{DispatchId, Error, Message, Result, Usage};
use flux_provider::{Effort, Provider};
use flux_runtime::{
    CompositeRegisterRequest, CompositeRegistrar, Executor, LoopHost, SkillLoadOutcome,
    SkillLoader, SpawnActivity, SpawnActivitySink, ToolProgress, ToolProgressSink, ToolRegistry,
    ToolResult,
};

use crate::composites::{prepare_registration, CompositeScope, DynamicComposites};
use crate::model::StageOptions;
use crate::registry::OpRegistry;
use crate::runtime::execute_flow_with_composites;
use crate::state::FlowStore;
use crate::AgentSink;

struct TurnContext {
    session_id: String,
    base_system: Option<String>,
    sink: Arc<Mutex<dyn AgentSink>>,
    advertised: Option<HashSet<String>>,
    registry: Arc<ToolRegistry>,
    audit: Option<(Arc<flux_events::EventStore>, i64)>,
}

/// The authored adaptive loop intentionally receives stage results as values. This retains the
/// exact tagged error value it received so the engine can make the finished turn machine-failing
/// after the loop has rendered the same apologetic human answer.
#[derive(Clone, Debug)]
pub(crate) struct StageFailure {
    pub(crate) kind: String,
    pub(crate) text: String,
}

/// Runtime capabilities used by the authored outer loop.
pub struct EngineLoopHost {
    executor: Weak<Executor>,
    provider: Mutex<Arc<dyn Provider>>,
    model: Mutex<String>,
    store: Arc<FlowStore>,
    composites: Arc<DynamicComposites>,
    options: Mutex<StageOptions>,
    turn: Mutex<TurnContext>,
    usage: Mutex<Usage>,
    calls: Mutex<Vec<(String, Usage)>>,
    stage_failure: Mutex<Option<StageFailure>>,
    /// A-96: per-turn call counter for the `consult` op's own budget (distinct from `calls`, which
    /// tallies billed usage for every independent model call regardless of source). Reset to zero
    /// in [`Self::set_turn`] like the rest of turn accounting.
    consult_calls: AtomicU32,
    token_budget: Mutex<Option<u64>>,
    adaptive_policy: Mutex<crate::staged::AdaptiveLoopPolicy>,
    conversation_cache: Mutex<HashMap<String, (Vec<Message>, i64)>>,
    receipts: crate::staged::ReceiptBook,
    groups: Mutex<Vec<flux_evidence::ToolGroup>>,
    model_stages: Mutex<BTreeMap<String, crate::staged::ModelStageDefinition>>,
    /// Active `run_authored_flow` reentry depth, guarding against runaway `flow_run` recursion
    /// (flow → `flow_run` → flow → …). Mirrors the reentry cap the retired `run_plan` host held.
    authored_depth: AtomicU32,
    /// D-188: the opt-in model-invoked skill catalog (discovered, non-`disable-model-invocation`
    /// skills) — empty means the mode is off for this engine. Set once via
    /// [`Self::set_skill_catalog`], read by both `skill.load` (through the [`SkillLoader`]
    /// capability) and the engine's per-turn prompt/surfacing assembly.
    skill_catalog: Mutex<Vec<flux_skill::Skill>>,
    /// D-188: names loaded via `skill.load` this session, keyed by session id. A loaded skill's
    /// full body is injected on every subsequent turn of that session — the same treatment an
    /// explicitly `--skill`-activated one gets — so activation has one consistent semantics
    /// regardless of how it happened.
    loaded_skills: Mutex<HashMap<String, HashSet<String>>>,
    /// A-94: the surface-shared mid-turn steering queue, drained by the adaptive loop at each
    /// planner-consultation round head. `None` (every non-interactive caller) is a no-op.
    steering: Mutex<Option<Arc<crate::steering::SteeringQueue>>>,
}

/// Hard cap on authored-flow reentry (`run_authored_flow`). A stored flow that calls `flow_run` on
/// itself — or a mutually recursive pair — otherwise recurses until the task stack is exhausted.
const MAX_AUTHORED_FLOW_DEPTH: u32 = 16;

/// RAII decrement for [`EngineLoopHost::authored_depth`], so the counter unwinds on every return
/// path (including the over-cap error and a panic).
struct AuthoredDepthGuard<'a>(&'a AtomicU32);

impl Drop for AuthoredDepthGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl EngineLoopHost {
    /// Install the host on the executor while retaining only a weak back-reference.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn install(
        mut executor: Executor,
        provider: Arc<dyn Provider>,
        model: String,
        base_system: Option<String>,
        store: Arc<FlowStore>,
        composites: Arc<DynamicComposites>,
        session_id: String,
        sink: Arc<Mutex<dyn AgentSink>>,
        options: StageOptions,
    ) -> (Arc<Executor>, Arc<Self>) {
        let slot: Arc<Mutex<Option<Arc<Self>>>> = Arc::new(Mutex::new(None));
        let captured = slot.clone();
        let executor = Arc::new_cyclic(move |weak| {
            let host = Arc::new(Self {
                executor: weak.clone(),
                provider: Mutex::new(provider),
                model: Mutex::new(model),
                store,
                composites,
                options: Mutex::new(options),
                turn: Mutex::new(TurnContext {
                    session_id,
                    base_system,
                    sink,
                    advertised: None,
                    registry: Arc::new(executor.registry().clone()),
                    audit: None,
                }),
                usage: Mutex::new(Usage::default()),
                calls: Mutex::new(Vec::new()),
                stage_failure: Mutex::new(None),
                consult_calls: AtomicU32::new(0),
                token_budget: Mutex::new(None),
                adaptive_policy: Mutex::new(crate::staged::AdaptiveLoopPolicy::default()),
                conversation_cache: Mutex::new(HashMap::new()),
                receipts: crate::staged::ReceiptBook::default(),
                groups: Mutex::new(Vec::new()),
                model_stages: Mutex::new(BTreeMap::new()),
                authored_depth: AtomicU32::new(0),
                skill_catalog: Mutex::new(Vec::new()),
                loaded_skills: Mutex::new(HashMap::new()),
                steering: Mutex::new(None),
            });
            *captured.lock().unwrap() = Some(host.clone());
            executor.set_loop_host(host.clone());
            executor.set_composite_registrar(host.clone());
            executor.set_skill_loader(host);
            executor
        });
        let host = slot.lock().unwrap().take().expect("loop host captured");
        (executor, host)
    }

    pub fn set_reasoning(&self, thinking: bool, effort: Option<Effort>) {
        let mut options = self.options.lock().unwrap();
        options.thinking = thinking;
        options.effort = effort;
    }

    pub fn set_groups(&self, groups: Vec<flux_evidence::ToolGroup>) {
        *self.groups.lock().unwrap() = groups;
    }

    /// Set the opt-in model-invoked skill catalog (D-188). Empty (the default) means the mode is
    /// off; a non-empty catalog is what makes `skill.load` surface and the compact name+description
    /// listing appear in the system prompt.
    pub fn set_skill_catalog(&self, catalog: Vec<flux_skill::Skill>) {
        *self.skill_catalog.lock().unwrap() = catalog;
    }

    /// The current opt-in model-invoked skill catalog (a clone — callers read this once per turn).
    pub fn skill_catalog(&self) -> Vec<flux_skill::Skill> {
        self.skill_catalog.lock().unwrap().clone()
    }

    /// Names loaded via `skill.load` so far in `session_id` (D-188). Empty when the mode is off or
    /// nothing has been loaded yet.
    pub fn loaded_skill_names(&self, session_id: &str) -> HashSet<String> {
        self.loaded_skills
            .lock()
            .unwrap()
            .get(session_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn set_model_stages(&self, stages: BTreeMap<String, crate::staged::ModelStageDefinition>) {
        *self.model_stages.lock().unwrap() = stages;
    }

    pub fn set_model(&self, provider: Arc<dyn Provider>, model: String) {
        *self.provider.lock().unwrap() = provider;
        *self.model.lock().unwrap() = model;
    }

    pub fn set_token_budget(&self, budget: Option<u64>) {
        *self.token_budget.lock().unwrap() = budget;
    }

    pub fn set_adaptive_policy(&self, policy: crate::staged::AdaptiveLoopPolicy) {
        *self.adaptive_policy.lock().unwrap() = policy;
    }

    /// A-94: install (or remove) the surface-shared mid-turn steering queue. The adaptive loop
    /// drains it at the head of every planner-consultation round; the same queue instance stays
    /// attached across turns so a message queued at a turn boundary is never lost.
    pub fn set_steering(&self, queue: Option<Arc<crate::steering::SteeringQueue>>) {
        *self.steering.lock().unwrap() = queue;
    }

    /// Point the long-lived host at the active turn and reset every turn-scoped capability.
    ///
    /// The returned reporter is the turn-owned capability that must be installed on the lexical
    /// [`flux_runtime::RuntimeTurnContext`]. It is intentionally not retained on this long-lived
    /// host/executor: doing so would let parallel or later turns observe the wrong child channel.
    #[must_use = "install the returned reporter on this turn's RuntimeTurnContext"]
    pub fn set_turn(
        &self,
        session_id: String,
        base_system: Option<String>,
        sink: Arc<Mutex<dyn AgentSink>>,
        advertised: Option<HashSet<String>>,
        registry: Arc<ToolRegistry>,
        audit: Option<(Arc<flux_events::EventStore>, i64)>,
    ) -> Arc<dyn SpawnActivitySink> {
        // The caller carries this reporter in its lexical RuntimeTurnContext. Never store it on the
        // long-lived executor: a retained context must not keep an obsolete turn channel, and two
        // independent turn futures must not retarget one another.
        let reporter: Arc<dyn SpawnActivitySink> =
            Arc::new(AgentSinkSpawnActivitySink(sink.clone()));
        self.conversation_cache
            .lock()
            .unwrap()
            .retain(|key, _| *key == session_id);
        *self.turn.lock().unwrap() = TurnContext {
            session_id,
            base_system,
            sink,
            advertised,
            registry,
            audit,
        };
        *self.usage.lock().unwrap() = Usage::default();
        self.calls.lock().unwrap().clear();
        *self.stage_failure.lock().unwrap() = None;
        self.consult_calls.store(0, Ordering::SeqCst);
        self.receipts.clear();
        reporter
    }

    pub fn turn_usage(&self) -> Usage {
        self.usage.lock().unwrap().clone()
    }

    pub fn turn_calls(&self) -> Vec<(String, Usage)> {
        self.calls.lock().unwrap().clone()
    }

    /// Take the first tagged stage failure observed in this turn. `set_turn` clears the slot, so a
    /// cancellation or later turn can never inherit an earlier provider failure.
    pub(crate) fn take_stage_failure(&self) -> Option<StageFailure> {
        self.stage_failure.lock().unwrap().take()
    }

    fn carry_stage_failure(&self, value: &Value) {
        let Some(kind) = value.get("kind").and_then(Value::as_str) else {
            return;
        };
        let Some(text) = value.get("text").and_then(Value::as_str) else {
            return;
        };
        let mut failure = self.stage_failure.lock().unwrap();
        if failure.is_none() {
            *failure = Some(StageFailure {
                kind: kind.to_string(),
                text: text.to_string(),
            });
        }
    }

    pub(crate) fn record_external_call(&self, provider: &str, model: &str, usage: Usage) {
        self.usage.lock().unwrap().accumulate(&usage);
        self.calls.lock().unwrap().push((
            flux_core::canonical_model_spec(Some(provider), model),
            usage,
        ));
    }

    /// Record a model call whose prompt is independent from the adaptive conversation. Cognition
    /// ops each build a fresh single-shot request, so their input/cache counters add rather than
    /// replacing the previous stage call's context snapshot.
    fn record_independent_call(&self, provider: &str, model: &str, usage: Usage) {
        self.usage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sum_independent(&usage);
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((
                flux_core::canonical_model_spec(Some(provider), model),
                usage,
            ));
    }

    fn record_stage_usages(&self, provider: &str, model: &str, usages: Vec<Usage>) {
        for usage in usages {
            self.record_external_call(provider, model, usage);
        }
    }

    fn cumulative_billed_tokens(&self) -> u64 {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|(_, usage)| usage.total())
            .sum()
    }

    fn executor(&self) -> Result<Arc<Executor>> {
        self.executor
            .upgrade()
            .ok_or_else(|| Error::Other("loop host: the executor is no longer alive".into()))
    }

    fn adaptive_context(&self) -> Result<(crate::staged::StagedContext, String, String)> {
        let executor = self.executor()?;
        let provider = self.provider.lock().unwrap().clone();
        let provider_name = provider.name().to_string();
        let model = self.model.lock().unwrap().clone();
        let options = self.options.lock().unwrap().clone();
        let groups = self.groups.lock().unwrap().clone();
        let adaptive_policy = self.adaptive_policy.lock().unwrap().clone();
        let (session_id, base_system, sink, audit, registry, mut advertised) = {
            let turn = self.turn.lock().unwrap();
            (
                turn.session_id.clone(),
                turn.base_system.clone(),
                turn.sink.clone(),
                turn.audit.clone(),
                turn.registry.clone(),
                turn.advertised.clone().unwrap_or_else(|| {
                    turn.registry
                        .specs()
                        .into_iter()
                        .map(|spec| spec.name)
                        .collect()
                }),
            )
        };
        advertised.retain(|name| executor.operation_visible(name));
        let conversation = self.load_persisted_conversation(&session_id);
        let remaining_token_budget = self
            .token_budget
            .lock()
            .unwrap()
            .map(|budget| budget.saturating_sub(self.cumulative_billed_tokens()));
        Ok((
            crate::staged::StagedContext {
                provider,
                model: model.clone(),
                executor,
                registry,
                store: self.store.clone(),
                session_id,
                conversation,
                base_system,
                sink,
                audit,
                advertised,
                authored_ceiling: None,
                groups,
                opts: options,
                remaining_token_budget,
                adaptive_policy,
                steering: self.steering.lock().unwrap().clone(),
            },
            provider_name,
            model,
        ))
    }

    /// Incrementally fold the append-only conversation stream, resetting on compaction events.
    pub(crate) fn load_persisted_conversation(&self, session_id: &str) -> Vec<Message> {
        let mut cache = self.conversation_cache.lock().unwrap();
        let (messages, cursor) = cache
            .entry(session_id.to_string())
            .or_insert_with(|| (Vec::new(), -1));
        match self.store.conversation_delta(session_id, *cursor) {
            Ok(delta) => {
                for event in &delta {
                    match &event.kind {
                        flux_events::EventKind::Message(message) => messages.push(message.clone()),
                        flux_events::EventKind::Compacted {
                            messages: compacted,
                        } => {
                            messages.clear();
                            messages.extend(compacted.iter().cloned());
                        }
                        _ => {}
                    }
                    *cursor = (*cursor).max(event.stream_seq);
                }
                messages.clone()
            }
            Err(_) => self.store.conversation(session_id).unwrap_or_default(),
        }
    }

    async fn run_scoped_segment(&self, input: Value) -> Result<Value> {
        let goal = input
            .get("goal")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .ok_or_else(|| Error::Other("ai_segment: non-empty 'goal' is required".into()))?;
        let tools = input
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::Other("ai_segment: 'tools' must be an array".into()))?
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_string)
                    .ok_or_else(|| Error::Other("ai_segment: every tool must be a string".into()))
            })
            .collect::<Result<HashSet<_>>>()?;
        if tools.is_empty() {
            return Err(Error::Other(
                "ai_segment: at least one scoped tool is required".into(),
            ));
        }
        let max_rounds = input
            .get("max_rounds")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .filter(|value| *value > 0)
            .ok_or_else(|| {
                Error::Other("ai_segment: 'max_rounds' must be greater than zero".into())
            })?;
        let current_turn = input
            .get("current_turn")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let max_tokens = input
            .get("max_tokens")
            .map(|value| {
                value
                    .as_u64()
                    .and_then(|value| u32::try_from(value).ok())
                    .filter(|value| *value > 0)
                    .ok_or_else(|| {
                        Error::Other("ai_segment: 'max_tokens' must be greater than zero".into())
                    })
            })
            .transpose()?;
        let max_history_bytes = input
            .get("max_history_bytes")
            .map(|value| {
                value
                    .as_u64()
                    .and_then(|value| usize::try_from(value).ok())
                    .filter(|value| *value > 0)
                    .ok_or_else(|| {
                        Error::Other(
                            "ai_segment: 'max_history_bytes' must be greater than zero".into(),
                        )
                    })
            })
            .transpose()?;

        let (mut context, provider, model) = self.adaptive_context()?;
        let effective_goal = if current_turn {
            let request = context
                .conversation
                .iter()
                .rev()
                .find(|message| message.role == flux_core::Role::User)
                .map(Message::text)
                .map(|request| request.trim().to_string())
                .filter(|request| !request.is_empty())
                .ok_or_else(|| {
                    Error::Other(
                        "ai_segment: current_turn requested but the turn has no user message"
                            .into(),
                    )
                })?;
            format!("{goal}\n\nCurrent request:\n{request}")
        } else {
            goal
        };
        let unavailable = tools
            .iter()
            .filter(|name| !context.advertised.contains(*name))
            .cloned()
            .collect::<Vec<_>>();
        if !unavailable.is_empty() {
            return Err(Error::Other(format!(
                "ai_segment: tools outside the live capability ceiling: {}",
                unavailable.join(", ")
            )));
        }
        context.advertised = tools;
        context.authored_ceiling = Some(context.advertised.clone());
        // Authored segments never inherit retained conversation implicitly. `current_turn` above
        // copies only the latest request into this fresh one-message segment.
        context.conversation = vec![Message::user_text(&effective_goal)];
        context.adaptive_policy.max_model_calls = max_rounds;
        context.adaptive_policy.explore.max_calls = None;
        // Like `max_rounds`, an authored ceiling wins outright: the author knows how much evidence
        // this segment legitimately accumulates.
        context.adaptive_policy.max_history_bytes = max_history_bytes;
        if let Some(max_tokens) = max_tokens {
            context.adaptive_policy.explore.max_tokens =
                Some(max_tokens.min(context.opts.max_tokens.max(1)));
        }
        let intent = crate::staged::scoped_segment_state(&context, &effective_goal)?;
        let mut next = json!({ "state": intent["state"].clone() });
        let mut remaining = max_rounds;

        while remaining > 0 {
            let run = crate::staged::explore_stage(context.clone(), next).await;
            let spent = run.usages.len().max(1);
            remaining = remaining.saturating_sub(spent);
            self.record_stage_usages(&provider, &model, run.usages);
            let step = run.result?;
            match step.get("kind").and_then(Value::as_str) {
                Some("chat") => {
                    return Ok(json!({
                        "result": step.get("text").and_then(Value::as_str).unwrap_or_default(),
                        "state": step.get("state").cloned().unwrap_or(Value::Null),
                    }));
                }
                Some("decision") => {
                    let rendered = self
                        .present_results(json!({ "step": step.clone() }))
                        .await?;
                    return Ok(json!({
                        "result": rendered.as_str().unwrap_or_default(),
                        "decision": step.get("question").cloned().unwrap_or(Value::Null),
                        "state": step.get("state").cloned().unwrap_or(Value::Null),
                    }));
                }
                Some("batch") => {
                    let batch = step.get("batch").cloned().ok_or_else(|| {
                        Error::Other("ai_segment: batch stage omitted batch".into())
                    })?;
                    let receipt = self
                        .approve_batch(json!({ "batch": batch.clone() }))
                        .await?;
                    if receipt.get("approved").and_then(Value::as_bool) != Some(true) {
                        return Ok(json!({
                            "result": receipt.get("summary").and_then(Value::as_str).unwrap_or(
                                "The proposed actions were not approved; nothing was executed."
                            ),
                            "state": step.get("state").cloned().unwrap_or(Value::Null),
                        }));
                    }
                    let report = self
                        .execute_batch(json!({ "batch": batch, "receipt": receipt }))
                        .await?;
                    next = json!({
                        "state": step.get("state").cloned().unwrap_or(Value::Null),
                        "report": report,
                    });
                }
                Some("error") => {
                    return Err(Error::Other(
                        step.get("text")
                            .and_then(Value::as_str)
                            .unwrap_or("ai_segment failed")
                            .to_string(),
                    ));
                }
                other => {
                    return Err(Error::Other(format!(
                        "ai_segment: unexpected adaptive result {other:?}"
                    )));
                }
            }
        }

        Err(Error::Other(format!(
            "ai_segment exhausted its {max_rounds} model-round budget before completion"
        )))
    }
}

fn decode_host_json(value: Value, label: &str) -> Result<Value> {
    match value {
        Value::String(text) => serde_json::from_str(&text)
            .map_err(|error| Error::Other(format!("{label}: expected JSON value: {error}"))),
        other => Ok(other),
    }
}

/// Stable wire label for a resumable authored-flow failure.
pub(crate) fn failure_kind_label(kind: crate::ast::FailureKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "runtime".to_string())
}

fn validate_live_batch(
    batch: &crate::staged::ActionBatch,
    registry: &flux_runtime::ToolRegistry,
) -> Result<()> {
    let mut ids = HashSet::new();
    for action in &batch.actions {
        if action.id.trim().is_empty() || !ids.insert(action.id.as_str()) {
            return Err(Error::Other(
                "action batch IDs must be non-empty and unique".into(),
            ));
        }
        let tool = registry.get(&action.op).ok_or_else(|| {
            Error::Other(format!(
                "action batch references unavailable operation '{}'",
                action.op
            ))
        })?;
        let spec = tool.spec();
        let validator = jsonschema::validator_for(&spec.input_schema).map_err(|error| {
            Error::Other(format!(
                "registered schema for '{}' is invalid: {error}",
                action.op
            ))
        })?;
        let diagnostics = validator
            .iter_errors(&action.input)
            .take(4)
            .map(|error| error.to_string())
            .collect::<Vec<_>>();
        if !diagnostics.is_empty() {
            return Err(Error::Other(format!(
                "invalid action '{}' input: {}",
                action.op,
                diagnostics.join("; ")
            )));
        }
    }
    Ok(())
}

#[async_trait]
impl LoopHost for EngineLoopHost {
    fn record_model_usage(&self, provider: &str, model: &str, usage: Usage) {
        self.record_independent_call(provider, model, usage);
    }

    fn reserve_consult_call(&self) -> usize {
        self.consult_calls.fetch_add(1, Ordering::SeqCst) as usize
    }

    async fn detect_intent(&self) -> Result<Value> {
        let (context, provider, _) = self.adaptive_context()?;
        let run = crate::staged::detect_intent_stage(context).await;
        self.record_stage_usages(&provider, &run.model, run.usages);
        match run.result {
            Ok(value) => Ok(value),
            Err(error) => {
                let value = json!({
                    "kind": "error",
                    "text": format!("Intent detection failed: {error}"),
                });
                self.carry_stage_failure(&value);
                Ok(value)
            }
        }
    }

    async fn explore(&self, input: Value) -> Result<Value> {
        let (context, provider, _) = self.adaptive_context()?;
        let run = crate::staged::explore_stage(context, input).await;
        self.record_stage_usages(&provider, &run.model, run.usages);
        match run.result {
            Ok(value) => Ok(value),
            Err(error) => {
                let value = json!({
                    "kind": "error",
                    "text": format!("Exploration failed: {error}"),
                });
                self.carry_stage_failure(&value);
                Ok(value)
            }
        }
    }

    async fn model_stage(&self, name: &str, input: Value) -> Result<Value> {
        let definition = self
            .model_stages
            .lock()
            .unwrap()
            .get(name)
            .cloned()
            .ok_or_else(|| Error::Other(format!("model stage '{name}' is not configured")))?;
        let (context, provider, _) = self.adaptive_context()?;
        let run = crate::staged::run_model_stage(context, name, definition, input).await;
        self.record_stage_usages(&provider, &run.model, run.usages);
        run.result
    }

    async fn run_authored_flow(&self, ast: Value) -> Result<Value> {
        // Bound reentry BEFORE any work: `flow_run` delegates here, and an authored flow that runs
        // itself (or a mutually recursive pair) would otherwise recurse until the stack is exhausted.
        let depth = self.authored_depth.fetch_add(1, Ordering::SeqCst);
        let _depth_guard = AuthoredDepthGuard(&self.authored_depth);
        if depth >= MAX_AUTHORED_FLOW_DEPTH {
            return Err(Error::Other(format!(
                "run_authored_flow: authored-flow reentry depth exceeded {MAX_AUTHORED_FLOW_DEPTH} \
                 (a recursive or mutually recursive `flow_run` cycle?)"
            )));
        }
        let ast: crate::ast::DraftAst = serde_json::from_value(ast)
            .map_err(|error| Error::Other(format!("run_authored_flow: invalid AST: {error}")))?;
        let executor = self.executor()?;
        let (session_id, sink, audit, turn_registry) = {
            let turn = self.turn.lock().unwrap();
            (
                turn.session_id.clone(),
                turn.sink.clone(),
                turn.audit.clone(),
                turn.registry.clone(),
            )
        };
        self.composites
            .ensure_session_loaded(&self.store, &session_id)?;
        let composites = self.composites.active_for_session(&session_id);
        let registry = OpRegistry::new(&turn_registry).with_owned_composites(composites.clone());
        let defined = self
            .store
            .view(&session_id)?
            .symbols
            .into_iter()
            .map(|symbol| symbol.name.0)
            .collect::<HashSet<_>>();
        flux_lang::analyze::lower(&ast, &registry, &defined).map_err(|diagnostics| {
            Error::Other(format!(
                "run_authored_flow: validation failed: {}",
                diagnostics
                    .into_iter()
                    .map(|diagnostic| diagnostic.message)
                    .collect::<Vec<_>>()
                    .join("; ")
            ))
        })?;
        if let Some((events, turn_id)) = &audit {
            crate::staged::record_host_flow_with(
                events,
                *turn_id,
                &session_id,
                &executor,
                &ast,
                "authored",
                1,
            );
        }
        let mut output = SharedSink::new(sink);
        let outcome = execute_flow_with_composites(
            &self.store,
            &executor,
            &session_id,
            &ast,
            &composites,
            &mut output,
        )
        .await
        .map_err(|error| Error::Other(error.to_string()))?;
        Ok(json!({
            "result": outcome.result,
            "transcript": outcome.transcript,
            "steps": outcome.steps,
            "suspension": outcome.suspension.map(|suspension| json!({
                "node": suspension.node.0,
                "source": suspension.source,
            })),
        }))
    }

    async fn approve_batch(&self, input: Value) -> Result<Value> {
        let executor = self.executor()?;
        let batch_value = input.get("batch").cloned().unwrap_or(input);
        let batch: crate::staged::ActionBatch =
            serde_json::from_value(decode_host_json(batch_value, "approve_batch.batch")?).map_err(
                |error| Error::Other(format!("approve_batch: invalid ActionBatch: {error}")),
            )?;
        let (session_id, sink, turn_registry) = {
            let turn = self.turn.lock().unwrap();
            (
                turn.session_id.clone(),
                turn.sink.clone(),
                turn.registry.clone(),
            )
        };
        if batch.session_id != session_id || batch.actions.is_empty() {
            return Err(Error::Other(
                "approve_batch: batch is empty or belongs to a different session".into(),
            ));
        }
        validate_live_batch(&batch, &turn_registry)?;

        let ast = crate::staged::action_batch_ast(&batch);
        let risk = crate::runtime::plan_risk(&ast, &turn_registry);
        let redacted = executor
            .context()
            .redactor
            .redact(&serde_json::to_string(&batch).unwrap_or_default());
        let proposed = flux_evidence::Observation::new(
            "action_batch.proposed",
            flux_evidence::Phase::Turn,
            json!({
                "batch_id": batch.id,
                "actions": batch.actions.len(),
                "risk": risk.summary(),
                "batch": redacted,
            }),
        );
        executor.observe(proposed.clone());
        SharedSink::new(sink.clone()).observation(&proposed);

        let requested = flux_evidence::Observation::new(
            "approval.requested",
            flux_evidence::Phase::Turn,
            json!({
                "scope": "action_batch",
                "batch_id": batch.id,
                "actions": batch.actions.len(),
                "risk": risk.summary(),
            }),
        );
        executor.observe(requested.clone());
        SharedSink::new(sink.clone()).observation(&requested);
        let approval_started = std::time::Instant::now();
        let approved_choice = executor
            .request_plan_approval(&risk.approval_request())
            .await;
        let approval_wait_us = approval_started.elapsed().as_micros().min(u64::MAX as u128) as u64;
        if !approved_choice {
            let denied = flux_evidence::Observation::new(
                "approval.denied",
                flux_evidence::Phase::Turn,
                json!({
                    "scope": "action_batch",
                    "batch_id": batch.id,
                    "wait_us": approval_wait_us,
                }),
            );
            executor.observe(denied.clone());
            SharedSink::new(sink).observation(&denied);
            return serde_json::to_value(crate::staged::ApprovalReceipt {
                approved: false,
                token: String::new(),
                batch_id: batch.id,
                destructive: risk.destructive,
                summary: "The action batch was not approved; no action was executed.".into(),
            })
            .map_err(Into::into);
        }

        let approved = flux_evidence::Observation::new(
            "approval.approved",
            flux_evidence::Phase::Turn,
            json!({
                "scope": "action_batch",
                "batch_id": batch.id,
                "wait_us": approval_wait_us,
            }),
        );
        executor.observe(approved.clone());
        SharedSink::new(sink).observation(&approved);
        let mut receipt = self.receipts.issue(
            &batch,
            &session_id,
            &executor.approval_context(),
            risk.destructive,
        );
        receipt.summary = risk.summary();
        serde_json::to_value(receipt).map_err(Into::into)
    }

    async fn execute_batch(&self, input: Value) -> Result<Value> {
        let executor = self.executor()?;
        let batch: crate::staged::ActionBatch = serde_json::from_value(decode_host_json(
            input
                .get("batch")
                .cloned()
                .ok_or_else(|| Error::Other("execute_batch: missing 'batch'".into()))?,
            "execute_batch.batch",
        )?)
        .map_err(|error| Error::Other(format!("execute_batch: invalid ActionBatch: {error}")))?;
        let receipt: crate::staged::ApprovalReceipt = serde_json::from_value(decode_host_json(
            input
                .get("receipt")
                .cloned()
                .ok_or_else(|| Error::Other("execute_batch: missing 'receipt'".into()))?,
            "execute_batch.receipt",
        )?)
        .map_err(|error| {
            Error::Other(format!("execute_batch: invalid ApprovalReceipt: {error}"))
        })?;
        let (session_id, sink, audit, turn_registry) = {
            let turn = self.turn.lock().unwrap();
            (
                turn.session_id.clone(),
                turn.sink.clone(),
                turn.audit.clone(),
                turn.registry.clone(),
            )
        };
        validate_live_batch(&batch, &turn_registry)?;
        let destructive =
            self.receipts
                .consume(&batch, &receipt, &session_id, &executor.approval_context())?;

        let _approved = executor.enter_approved_scope(destructive);
        let execution_started = std::time::Instant::now();
        self.composites
            .ensure_session_loaded(&self.store, &session_id)?;
        let composites = self.composites.active_for_session(&session_id);
        let mut actions = Vec::with_capacity(batch.actions.len());
        let mut failed = false;
        for (index, action) in batch.actions.iter().enumerate() {
            if failed {
                actions.push(crate::staged::ActionResult {
                    id: action.id.clone(),
                    op: action.op.clone(),
                    status: "skipped".into(),
                    result: "not attempted after an earlier action failed".into(),
                });
                continue;
            }
            let ast = crate::staged::action_ast(&batch, action);
            if let Some((events, turn_id)) = &audit {
                crate::staged::record_host_flow_with(
                    events,
                    *turn_id,
                    &session_id,
                    &executor,
                    &ast,
                    "execute",
                    index as u32 + 1,
                );
            }
            let result = {
                let mut output = SharedSink::new(sink.clone());
                execute_flow_with_composites(
                    &self.store,
                    &executor,
                    &session_id,
                    &ast,
                    &composites,
                    &mut output,
                )
                .await
            };
            let (status, result) = match result {
                Ok(outcome) => ("ok", outcome.result),
                Err(flux_lang::error::FlowError::Denied(message)) => ("denied", message),
                Err(error) => ("error", error.to_string()),
            };
            failed = status != "ok";
            actions.push(crate::staged::ActionResult {
                id: action.id.clone(),
                op: action.op.clone(),
                status: status.into(),
                result,
            });
        }
        let report = crate::staged::ExecutionReport {
            batch_id: batch.id.clone(),
            ok: !failed,
            actions,
        };
        let duration_us = execution_started
            .elapsed()
            .as_micros()
            .min(u64::MAX as u128) as u64;
        let executed = flux_evidence::Observation::new(
            "action_batch.executed",
            flux_evidence::Phase::Turn,
            json!({
                "batch_id": batch.id,
                "ok": report.ok,
                "duration_us": duration_us,
                "actions": report.actions.iter().map(|action| json!({
                    "id": action.id,
                    "op": action.op,
                    "status": action.status,
                })).collect::<Vec<_>>(),
            }),
        );
        executor.observe(executed.clone());
        SharedSink::new(sink).observation(&executed);
        serde_json::to_value(report).map_err(Into::into)
    }

    async fn present_results(&self, input: Value) -> Result<Value> {
        if let Some(approval) = input.get("approval") {
            let approval = decode_host_json(approval.clone(), "present_results.approval")?;
            if approval.get("approved").and_then(Value::as_bool) == Some(false) {
                return Ok(Value::String(
                    approval
                        .get("summary")
                        .and_then(Value::as_str)
                        .unwrap_or("The proposed actions were not approved; nothing was executed.")
                        .to_string(),
                ));
            }
        }
        let step = decode_host_json(
            input.get("step").cloned().unwrap_or(input),
            "present_results.step",
        )?;
        if step.get("kind").and_then(Value::as_str) == Some("decision") {
            let question = step.get("question").unwrap_or(&Value::Null);
            let prompt = question
                .get("prompt")
                .and_then(Value::as_str)
                .unwrap_or("I need your decision before continuing.");
            let options = question
                .get("options")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .enumerate()
                        .map(|(index, value)| format!("{}. {value}", index + 1))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            return Ok(Value::String(if options.is_empty() {
                prompt.to_string()
            } else {
                format!("{prompt}\n{options}")
            }));
        }
        Ok(Value::String(
            step.get("text")
                .and_then(Value::as_str)
                .unwrap_or("The adaptive turn ended without a response.")
                .to_string(),
        ))
    }

    async fn ai_segment(&self, input: Value) -> Result<Value> {
        self.run_scoped_segment(input).await
    }
}

#[async_trait]
impl CompositeRegistrar for EngineLoopHost {
    async fn register_composite(&self, request: CompositeRegisterRequest) -> Result<Value> {
        let (scope, declaration, source, replace) = prepare_registration(request)?;
        let session_id = self.turn.lock().unwrap().session_id.clone();
        let executor = self.executor()?;
        let turn_registry = self.turn.lock().unwrap().registry.clone();
        self.composites
            .ensure_session_loaded(&self.store, &session_id)?;
        self.composites.validate_registration(
            scope,
            &session_id,
            &declaration,
            replace,
            &turn_registry,
        )?;

        let path = match scope {
            CompositeScope::Turn => None,
            CompositeScope::Session => {
                self.store
                    .save_session_composite(&session_id, &declaration.name, &source)?;
                None
            }
            CompositeScope::Project => {
                let path = scope.path_for(&declaration.name).expect("project path");
                executor
                    .context()
                    .system()
                    .write_file(&path, &source)
                    .await?;
                Some(path)
            }
            CompositeScope::Global => {
                if !executor
                    .context()
                    .system()
                    .workspace()
                    .has_named_root("global_ops")
                {
                    return Err(Error::Other(
                        "op.register: global scope is unavailable because @global_ops is not configured"
                            .into(),
                    ));
                }
                let path = scope.path_for(&declaration.name).expect("global path");
                executor
                    .context()
                    .system()
                    .write_file(&path, &source)
                    .await?;
                Some(path)
            }
        };
        self.composites
            .install(scope, &session_id, declaration.clone(), replace)?;
        executor.observe(flux_evidence::Observation::new(
            "op.registered",
            flux_evidence::Phase::Turn,
            json!({
                "name": declaration.name,
                "scope": scope.as_str(),
                "path": path,
                "replace": replace,
            }),
        ));
        Ok(json!({
            "name": declaration.name,
            "scope": scope.as_str(),
            "persisted": matches!(
                scope,
                CompositeScope::Session
                    | CompositeScope::Project
                    | CompositeScope::Global
            ),
            "path": path,
        }))
    }
}

#[async_trait]
impl SkillLoader for EngineLoopHost {
    /// Look `name` up in the opt-in catalog and record it loaded for `session_id`. Errors when the
    /// catalog is empty (the mode is off) or `name` isn't in it — including a skill that declared
    /// `disable-model-invocation: true`, which the caller never puts in the catalog in the first
    /// place (D-189's `Skill::disable_model_invocation`).
    async fn load_skill(&self, session_id: &str, name: &str) -> Result<SkillLoadOutcome> {
        let catalog = self.skill_catalog.lock().unwrap().clone();
        let skill = catalog
            .into_iter()
            .find(|s| s.name == name)
            .ok_or_else(|| {
                Error::Other(format!(
                    "skill.load: `{name}` is not a model-invocable skill (unknown, or it declares \
                 `disable-model-invocation: true`)"
                ))
            })?;
        self.loaded_skills
            .lock()
            .unwrap()
            .entry(session_id.to_string())
            .or_default()
            .insert(skill.name.clone());
        Ok(SkillLoadOutcome {
            name: skill.name,
            body: skill.body.text().to_string(),
        })
    }
}

/// RAII bracket for a model-stage consultation.
pub(crate) struct PlanningGuard(Arc<Mutex<dyn AgentSink>>);

impl PlanningGuard {
    pub(crate) fn start(sink: Arc<Mutex<dyn AgentSink>>) -> Self {
        SharedSink::new(sink.clone()).planning(true);
        Self(sink)
    }
}

impl Drop for PlanningGuard {
    fn drop(&mut self) {
        SharedSink::new(self.0.clone()).planning(false);
    }
}

/// Live sink proxy used by nested authored flows and model stages.
pub struct SharedSink(Arc<Mutex<dyn AgentSink>>);

impl SharedSink {
    pub fn new(sink: Arc<Mutex<dyn AgentSink>>) -> Self {
        Self(sink)
    }
}

impl AgentSink for SharedSink {
    fn text_delta(&mut self, text: &str) {
        self.0.lock().unwrap().text_delta(text);
    }

    fn thinking_delta(&mut self, text: &str) {
        self.0.lock().unwrap().thinking_delta(text);
    }

    fn planning(&mut self, active: bool) {
        self.0.lock().unwrap().planning(active);
    }

    fn tool_call(&mut self, dispatch: DispatchId, name: &str, input: &Value) {
        self.0.lock().unwrap().tool_call(dispatch, name, input);
    }

    fn tool_result(&mut self, dispatch: DispatchId, name: &str, result: &ToolResult) {
        self.0.lock().unwrap().tool_result(dispatch, name, result);
    }

    fn tool_timing(
        &mut self,
        dispatch: DispatchId,
        name: &str,
        timing: &flux_core::OperationTiming,
    ) {
        self.0.lock().unwrap().tool_timing(dispatch, name, timing);
    }

    fn observation(&mut self, observation: &flux_evidence::Observation) {
        self.0.lock().unwrap().observation(observation);
    }

    fn turn_end(&mut self, usage: Option<Usage>) {
        self.0.lock().unwrap().turn_end(usage);
    }
}

/// One sink call captured for forwarding onto a borrowed surface.
pub enum SinkEvent {
    Text(String),
    Thinking(String),
    Planning(bool),
    ToolCall(DispatchId, String, Value),
    ToolTiming(DispatchId, String, flux_core::OperationTiming),
    ToolResult(DispatchId, String, ToolResult),
    Observation(flux_evidence::Observation),
    TurnEnd(Option<Usage>),
}

impl SinkEvent {
    pub fn apply(self, sink: &mut dyn AgentSink) {
        match self {
            Self::Text(text) => sink.text_delta(&text),
            Self::Thinking(text) => sink.thinking_delta(&text),
            Self::Planning(active) => sink.planning(active),
            Self::ToolCall(dispatch, name, input) => sink.tool_call(dispatch, &name, &input),
            Self::ToolTiming(dispatch, name, timing) => sink.tool_timing(dispatch, &name, &timing),
            Self::ToolResult(dispatch, name, result) => sink.tool_result(dispatch, &name, &result),
            Self::Observation(observation) => sink.observation(&observation),
            Self::TurnEnd(usage) => sink.turn_end(usage),
        }
    }
}

/// Owned sink used while the engine concurrently drains onto a borrowed surface.
pub struct ChannelSink(tokio::sync::mpsc::UnboundedSender<SinkEvent>);

impl ChannelSink {
    pub fn new(sender: tokio::sync::mpsc::UnboundedSender<SinkEvent>) -> Self {
        Self(sender)
    }
}

impl AgentSink for ChannelSink {
    fn text_delta(&mut self, text: &str) {
        let _ = self.0.send(SinkEvent::Text(text.to_string()));
    }

    fn thinking_delta(&mut self, text: &str) {
        let _ = self.0.send(SinkEvent::Thinking(text.to_string()));
    }

    fn planning(&mut self, active: bool) {
        let _ = self.0.send(SinkEvent::Planning(active));
    }

    fn tool_call(&mut self, dispatch: DispatchId, name: &str, input: &Value) {
        let _ = self.0.send(SinkEvent::ToolCall(
            dispatch,
            name.to_string(),
            input.clone(),
        ));
    }

    fn tool_result(&mut self, dispatch: DispatchId, name: &str, result: &ToolResult) {
        let _ = self.0.send(SinkEvent::ToolResult(
            dispatch,
            name.to_string(),
            result.clone(),
        ));
    }

    fn tool_timing(
        &mut self,
        dispatch: DispatchId,
        name: &str,
        timing: &flux_core::OperationTiming,
    ) {
        let _ = self
            .0
            .send(SinkEvent::ToolTiming(dispatch, name.to_string(), *timing));
    }

    fn observation(&mut self, observation: &flux_evidence::Observation) {
        let _ = self.0.send(SinkEvent::Observation(observation.clone()));
    }

    fn turn_end(&mut self, usage: Option<Usage>) {
        let _ = self.0.send(SinkEvent::TurnEnd(usage));
    }
}

/// L3 adapter from the owned parent [`AgentSink`] channel to the L2 spawner callback. Emission is
/// synchronous and send-only (the wrapped [`ChannelSink`] just enqueues), so no lock crosses await.
struct AgentSinkSpawnActivitySink(Arc<Mutex<dyn AgentSink>>);

impl SpawnActivitySink for AgentSinkSpawnActivitySink {
    fn emit(&self, activity: SpawnActivity) {
        self.0
            .lock()
            .unwrap()
            .observation(&activity.to_observation());
    }
}

/// C-158 counterpart of [`AgentSinkSpawnActivitySink`] for a directly-invoked tool's own in-flight
/// output. Same shape and same constraint — emission only enqueues on the wrapped `ChannelSink`, so
/// nothing blocks the pipe-drain task that calls it.
pub(crate) struct AgentSinkToolProgressSink(pub(crate) Arc<Mutex<dyn AgentSink>>);

impl ToolProgressSink for AgentSinkToolProgressSink {
    fn emit(&self, progress: ToolProgress) {
        self.0
            .lock()
            .unwrap()
            .observation(&progress.to_observation());
    }
}
