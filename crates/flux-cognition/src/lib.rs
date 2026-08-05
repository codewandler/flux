//! `flux-cognition` — an L3, provider-injected pack of model-backed *cognition* ops.
//!
//! These ops turn the model into a typed, named operation the planner can call like any other
//! tool: `ai.extract`, `ai.rank`, `ai.judge`, `ai.reason`, `synth`, and `ai.rewrite`. Each one
//! owns an `Arc<dyn Provider>` plus a concrete `model` id and, on `execute`, assembles a prompt
//! from its params, streams a single-shot completion, and returns the model's text as the
//! [`ToolResult`]. For the extract/judge/synth ops the model is instructed to emit JSON and the
//! op passes that text through verbatim — parsing is the caller's job (the symbols-over-values
//! thesis: an op returns a value, the caller decides its shape).
//!
//! **Why this is a real op, not a prompt fragment.** Each op's [`spec`](flux_runtime::Tool::spec)
//! is built as a Flux-Lang [`OpSpec`] with *typed, named* [`Param`]s — artifact-typed slots use
//! [`TypeRef::Named`] (e.g. `Claim`, `Verdict`, `Answer`) — and lowered via [`OpSpec::lower`] to a
//! faithful named JSON Schema. That exercises P0 and yields planner signatures that read back
//! (`required`/`properties`) exactly like the built-in tools. A model call is a [`FlowEffect::Model`]
//! (semantic) that travels over the network, so the lowered host effect is [`Effect::Network`]; the
//! op declares [`AccessKind::Provider`] and [`Risk::Medium`] — `Medium` rather than `Low` because a
//! model call is *billable*, and `Risk::Low` is the tier that would let the adaptive loop spend the
//! operator's budget during pre-approval evidence gathering (C-208).
//!
//! The pack is L3 because it depends on the L1 [`Provider`] abstraction. It is *provider-injected*:
//! construct [`CognitionPack::new`] with whatever provider/model the host has wired up and call
//! [`CognitionPack::register`] to add every op to a [`ToolRegistry`].

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::Value;

use flux_core::{Chunk, Error, Result, Usage};
use flux_evidence::{EvidenceLog, Observation, Phase};
use flux_lang::ast::{FlowEffect, TypeRef};
use flux_lang::opspec::{OpSpec, Param};
use flux_provider::{Effort, Provider, Request};
use flux_runtime::{LoopHost, OperationPlacement, Tool, ToolContext, ToolRegistry, ToolResult};
use flux_spec::{AccessKind, Idempotency, Risk, ToolSpec};

mod consult;
pub use consult::{
    ConsultFactory, ConsultTool, CONSULT_USAGE_OBSERVATION_KIND, DEFAULT_CONSULT_MAX_CALLS,
};

/// Token budget for a single cognition completion. Generous enough for a synthesized answer or an
/// extracted array, bounded so a runaway generation can't burn the whole context.
const MAX_TOKENS: u32 = 4096;

/// Evidence kind emitted once for every cognition call that reports billable usage.
pub const USAGE_OBSERVATION_KIND: &str = "cognition.usage";

/// Sum the independent cognition calls recorded in an evidence log.
///
/// Direct SDK/App flow runners do not install a turn-owning [`LoopHost`], so their structured
/// result reads this side channel after execution. Each cognition op owns a fresh prompt; every
/// usage field therefore adds via [`Usage::sum_independent`] rather than the adaptive agent loop's
/// context-snapshot semantics.
pub fn recorded_usage(evidence: &EvidenceLog) -> Option<Usage> {
    let mut total = Usage::default();
    let mut any = false;
    for observation in evidence.by_kind(USAGE_OBSERVATION_KIND) {
        let Some(usage) = observation
            .data
            .get("usage")
            .and_then(|value| serde_json::from_value::<Usage>(value.clone()).ok())
        else {
            continue;
        };
        total.sum_independent(&usage);
        any = true;
    }
    any.then_some(total)
}

/// The six cognition ops. One enum keeps the [`Tool`] implementation DRY while still registering a
/// distinct, independently-dispatchable op per variant (each variant owns the same provider/model).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpKind {
    Extract,
    Rank,
    Judge,
    Reason,
    Synth,
    Rewrite,
}

impl OpKind {
    /// Every op, in a stable order — the set [`CognitionPack::register`] installs.
    const ALL: [OpKind; 6] = [
        OpKind::Extract,
        OpKind::Rank,
        OpKind::Judge,
        OpKind::Reason,
        OpKind::Synth,
        OpKind::Rewrite,
    ];

    /// The registered tool name. Cognition ops are namespaced with a `ai.` prefix except `synth`,
    /// which is the top-level synthesis op.
    fn name(self) -> &'static str {
        match self {
            OpKind::Extract => "ai.extract",
            OpKind::Rank => "ai.rank",
            OpKind::Judge => "ai.judge",
            OpKind::Reason => "ai.reason",
            OpKind::Synth => "synth",
            OpKind::Rewrite => "ai.rewrite",
        }
    }

    /// The typed Flux-Lang contract for this op. Lowered to a [`ToolSpec`] by [`CognitionOp::spec`].
    fn opspec(self) -> OpSpec {
        // A model call is a semantic `Model` effect that egresses over the network; pairing it with
        // `Network` is what makes the lowered host effect surface as `Effect::Network` (the `Model`
        // semantic carries a `model.invoke` policy action but no host effect of its own).
        let effects = vec![FlowEffect::Model, FlowEffect::Network];
        let (name, description, inputs, output): (&str, &str, Vec<Param>, TypeRef) = match self {
            OpKind::Extract => (
                "ai.extract",
                "Extract typed items from free text using the model. Returns a JSON array of the \
                 extracted items (e.g. a `Claim[]`); the model emits JSON and the op passes it \
                 through for the caller to parse.",
                vec![
                    required("from", TypeRef::String),
                    optional("ask", TypeRef::String),
                    optional("schema", TypeRef::String),
                ],
                TypeRef::List(Box::new(TypeRef::Named("Claim".into()))),
            ),
            OpKind::Rank => (
                "ai.rank",
                "Reorder a list of items by a natural-language criterion. Returns the same items as \
                 a JSON array, best-first.",
                vec![
                    required("items", TypeRef::List(Box::new(TypeRef::Any))),
                    optional("by", TypeRef::String),
                ],
                TypeRef::List(Box::new(TypeRef::Any)),
            ),
            OpKind::Judge => (
                "ai.judge",
                "Adjudicate a claim against evidence. Returns a Verdict-shaped JSON object \
                 `{ choice, reasons }`.",
                vec![
                    required("claim", TypeRef::Named("Claim".into())),
                    optional("evidence", TypeRef::Named("Evidence".into())),
                ],
                TypeRef::Named("Verdict".into()),
            ),
            OpKind::Reason => (
                "ai.reason",
                "Free-form reasoning over a context pack. Returns the model's answer as text.",
                vec![
                    optional("ctx", TypeRef::Named("Ctx".into())),
                    required("ask", TypeRef::String),
                ],
                TypeRef::String,
            ),
            OpKind::Synth => (
                "synth",
                "Synthesize a single cited Answer from a set of claims. Returns a JSON object \
                 `{ status, summary, evidence, gaps, risks }`.",
                vec![
                    required("claims", TypeRef::List(Box::new(TypeRef::Named("Claim".into())))),
                    optional("format", TypeRef::String),
                    optional("cite", TypeRef::Bool),
                ],
                TypeRef::Named("Answer".into()),
            ),
            OpKind::Rewrite => (
                "ai.rewrite",
                "Rewrite text in a requested style, preserving meaning. Returns the rewritten text.",
                vec![
                    required("text", TypeRef::String),
                    optional("style", TypeRef::String),
                ],
                TypeRef::String,
            ),
        };
        OpSpec {
            name: name.to_string(),
            description: description.to_string(),
            inputs,
            output,
            effects,
            // `Medium`, not `Low` (C-208). Every op in this pack spends money on a provider call.
            // `Risk::Low` would put them in the gather path, letting the adaptive loop bill the
            // operator during exploration, before anything was approved. Deliberately NOT resolved
            // by pairing `Network` with `Read`: what separates these from `web.fetch` is cost, not
            // mutation. See docs/designs/security-assurance.md.
            risk: Risk::Medium,
            // A model call is non-deterministic unless cached, so repeating it is not idempotent.
            idempotency: Idempotency::NonIdempotent,
        }
    }

    /// The system prompt that frames this op's role for the model.
    fn system(self) -> &'static str {
        match self {
            OpKind::Extract => {
                "You are a precise information-extraction engine. Extract exactly the requested \
                 items from the input text. Return ONLY a JSON array of objects and no other prose."
            }
            OpKind::Rank => {
                "You reorder a list of items by the given criterion. Return ONLY a JSON array \
                 containing exactly the same items, ordered best-first."
            }
            OpKind::Judge => {
                "You are a careful adjudicator. Decide whether the claim is supported by the \
                 evidence. Return ONLY a JSON object of the form \
                 {\"choice\": \"supported|refuted|insufficient\", \"reasons\": [\"...\"]}."
            }
            OpKind::Reason => {
                "You are a careful reasoning engine. Reason over the provided context and answer \
                 the question directly and concisely."
            }
            OpKind::Synth => {
                "You synthesize a single cited answer from a set of claims. Return ONLY a JSON \
                 object with the keys: status, summary, evidence, gaps, risks."
            }
            OpKind::Rewrite => {
                "You rewrite text in the requested style while preserving its meaning. Return ONLY \
                 the rewritten text, with no preamble."
            }
        }
    }

    /// Assemble the user prompt from the call params. Lenient with optional params (a missing one is
    /// simply omitted); errors only when the op's primary input is absent.
    fn prompt(self, params: &Value) -> Result<String> {
        let p = |key: &str| field_text(params, key);
        Ok(match self {
            OpKind::Extract => {
                let from = require(params, "from", self.name())?;
                let ask = p("ask");
                let schema = p("schema");
                let schema_block = if schema.is_empty() {
                    String::new()
                } else {
                    format!("Target item schema:\n{schema}\n\n")
                };
                format!(
                    "Task: {ask}\n\n{schema_block}Input text:\n{from}\n\nReturn a JSON array of the \
                     extracted items."
                )
            }
            OpKind::Rank => {
                let items = require(params, "items", self.name())?;
                let by = p("by");
                format!(
                    "Criterion: {by}\n\nItems (JSON):\n{items}\n\nReturn the items reordered \
                     best-first as a JSON array."
                )
            }
            OpKind::Judge => {
                let claim = require(params, "claim", self.name())?;
                let evidence = p("evidence");
                format!("Claim:\n{claim}\n\nEvidence:\n{evidence}\n\nReturn the verdict JSON.")
            }
            OpKind::Reason => {
                let ask = require(params, "ask", self.name())?;
                let ctx = context_text(params);
                format!("Context:\n{ctx}\n\nQuestion:\n{ask}")
            }
            OpKind::Synth => {
                let claims = require(params, "claims", self.name())?;
                let format = {
                    let f = p("format");
                    if f.is_empty() {
                        "markdown".to_string()
                    } else {
                        f
                    }
                };
                let cite = params.get("cite").and_then(Value::as_bool).unwrap_or(true);
                format!(
                    "Claims (JSON):\n{claims}\n\nOutput format: {format}\nInclude citations: \
                     {cite}\n\nReturn the answer JSON."
                )
            }
            OpKind::Rewrite => {
                let text = require(params, "text", self.name())?;
                let style = p("style");
                format!("Style: {style}\n\nText:\n{text}")
            }
        })
    }
}

/// One cognition op: a [`Tool`] backed by a provider/model. The [`OpKind`] selects its spec,
/// system framing, and prompt assembly; the rest of the [`Tool`] machinery is shared.
struct CognitionOp {
    kind: OpKind,
    provider: Arc<dyn Provider>,
    model: String,
    system_prefix: Option<String>,
    thinking: bool,
    effort: Option<Effort>,
}

#[async_trait]
impl Tool for CognitionOp {
    fn spec(&self) -> ToolSpec {
        // P0: lower the typed, named OpSpec to a faithful JSON-Schema ToolSpec, then layer on the
        // host facts the envelope gates on — a model call needs provider access (network egress).
        // The tier is NOT restated here. `OpSpec::lower` already carries `opspec()`'s `risk`, and a
        // second `.with_risk(…)` at this seam silently shadowed it — the typed contract said one
        // thing and the registered `ToolSpec` another. That is the exact drift shape C-191/C-208
        // exist to catch, so the declaration now lives in one place: `OpKind::opspec`.
        self.kind
            .opspec()
            .lower()
            .with_access(vec![AccessKind::Provider])
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let prompt = self.kind.prompt(&params)?;
        let system = match self.system_prefix.as_deref() {
            Some(prefix) if !prefix.trim().is_empty() => format!(
                "{prefix}\n\nOperation contract (follow this for the current call):\n{}",
                self.kind.system()
            ),
            _ => self.kind.system().to_string(),
        };
        // A retained engine context also retains its loop-host capability between turns. Publish
        // into turn accounting only while a lexical/stored session is active; direct executor
        // dispatches outside a turn still keep the evidence observation without contaminating the
        // host's last or next turn total.
        let turn_loop_host = ctx.session_id().and(ctx.loop_host.clone());
        let mut usage_observation = UsageObservationGuard::new(
            ctx.evidence.clone(),
            turn_loop_host,
            self.kind.name(),
            self.provider.name().to_string(),
            self.model.clone(),
        );
        let (result, usage) = run_model(
            self.provider.as_ref(),
            &self.model,
            &system,
            &prompt,
            self.thinking,
            self.effort,
            &mut usage_observation,
        )
        .await;
        // Usage is independent from the provider result: a declared stream failure after a usage
        // frame still cost tokens. Record before propagating the untouched error. The guard's Drop
        // path performs the same one-shot recording if cancellation drops this future mid-stream.
        usage_observation.finish(usage);
        let out = result?;
        Ok(ToolResult::ok(out))
    }
}

/// One cognition call's usage observation. Normal completion calls [`Self::finish`]; dropping an
/// in-flight operation (turn cancellation, timeout/race loser, or task abort) records the latest
/// usage snapshot already observed by the stream collector. `recorded` makes the two paths mutually
/// exclusive, so successful and failed calls cannot be double-counted.
struct UsageObservationGuard {
    evidence: Arc<std::sync::Mutex<EvidenceLog>>,
    loop_host: Option<Arc<dyn LoopHost>>,
    op: &'static str,
    provider: String,
    model: String,
    usage: Usage,
    recorded: bool,
}

impl UsageObservationGuard {
    fn new(
        evidence: Arc<std::sync::Mutex<EvidenceLog>>,
        loop_host: Option<Arc<dyn LoopHost>>,
        op: &'static str,
        provider: String,
        model: String,
    ) -> Self {
        Self {
            evidence,
            loop_host,
            op,
            provider,
            model,
            usage: Usage::default(),
            recorded: false,
        }
    }

    /// Provider usage chunks are cumulative within one call, so the last observed snapshot wins.
    fn observe(&mut self, usage: &Usage) {
        self.usage = usage.clone();
    }

    fn finish(&mut self, usage: Usage) {
        self.observe(&usage);
        self.record();
    }

    fn record(&mut self) {
        if self.recorded {
            return;
        }
        self.recorded = true;
        // Preserve the established D-150 behavior: a provider that reports no billable tokens
        // produces no observation, so downstream execution totals remain `None` rather than zero.
        if self.usage.total() == 0 {
            return;
        }
        let usage = self.usage.clone();
        let observation = Observation::new(
            USAGE_OBSERVATION_KIND,
            Phase::Turn,
            serde_json::json!({
                "op": self.op,
                "provider": &self.provider,
                "model": &self.model,
                "usage": &usage,
            }),
        );
        // Drop must never panic while another failure is unwinding. A poisoned evidence lock is
        // already unusable; leave the original provider/cancellation terminal state untouched.
        if let Ok(mut evidence) = self.evidence.lock() {
            evidence.record(observation);
        }
        if let Some(loop_host) = &self.loop_host {
            loop_host.record_model_usage(&self.provider, &self.model, usage);
        }
    }
}

impl Drop for UsageObservationGuard {
    fn drop(&mut self) {
        self.record();
    }
}

/// A provider-injected pack of model-backed cognition ops. Construct it with the provider/model the
/// host has configured, then [`try_register`](Self::try_register) every op into a [`ToolRegistry`].
pub struct CognitionPack {
    provider: Arc<dyn Provider>,
    model: String,
    system_prefix: Option<String>,
    thinking: bool,
    effort: Option<Effort>,
}

impl CognitionPack {
    /// Build a pack bound to `provider` and a concrete `model` id (already resolved from any alias).
    pub fn new(provider: Arc<dyn Provider>, model: impl Into<String>) -> Self {
        Self {
            provider,
            model: model.into(),
            system_prefix: None,
            thinking: false,
            effort: None,
        }
    }

    /// Prefix every cognition op's operation-specific system contract with an owning agent's
    /// persona. The operation contract remains last so JSON/text shape requirements stay explicit.
    pub fn with_system_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.system_prefix = Some(prefix.into());
        self
    }

    /// Apply the owning agent's adaptive-thinking and reasoning-effort policy to every cognition
    /// request in this pack.
    pub fn with_reasoning(mut self, thinking: bool, effort: Option<Effort>) -> Self {
        self.thinking = thinking;
        self.effort = effort;
        self
    }

    /// Stable operation names installed by this pack.
    pub fn names() -> impl Iterator<Item = &'static str> {
        OpKind::ALL.into_iter().map(OpKind::name)
    }

    /// Register every cognition op (`ai.extract`, `ai.rank`, `ai.judge`, `ai.reason`, `synth`,
    /// `ai.rewrite`) into `registry`. Each op shares this pack's provider and model.
    pub fn try_register(&self, registry: &mut ToolRegistry) -> Result<()> {
        self.try_register_from("flux-cognition model operation pack", registry)
    }

    /// Register this pack with an explicit assembly-source label.
    pub fn try_register_from(
        &self,
        source: impl Into<String>,
        registry: &mut ToolRegistry,
    ) -> Result<()> {
        let tools = OpKind::ALL.into_iter().map(|kind| {
            Arc::new(CognitionOp {
                kind,
                provider: self.provider.clone(),
                model: self.model.clone(),
                system_prefix: self.system_prefix.clone(),
                thinking: self.thinking,
                effort: self.effort,
            }) as Arc<dyn Tool>
        });
        registry.try_register_all_from_with_placement(
            source,
            tools,
            OperationPlacement::LocalControlPlane,
        )
    }

    /// Intentionally replace this pack's canonical operation family.
    ///
    /// App uses this when a declared agent overrides the host model/persona. The operation names
    /// are fixed by [`OpKind`], and the source label makes the otherwise-dangerous replacement
    /// visible in registry diagnostics.
    pub fn replace_from(
        &self,
        source: impl Into<String>,
        registry: &mut ToolRegistry,
    ) -> Result<()> {
        let source = source.into();
        let mut assembled = registry.clone();
        for kind in OpKind::ALL {
            assembled.replace_from_with_placement(
                source.clone(),
                Arc::new(CognitionOp {
                    kind,
                    provider: self.provider.clone(),
                    model: self.model.clone(),
                    system_prefix: self.system_prefix.clone(),
                    thinking: self.thinking,
                    effort: self.effort,
                }),
                OperationPlacement::LocalControlPlane,
            )?;
        }
        *registry = assembled;
        Ok(())
    }

    /// Compatibility wrapper for callers that cannot yet propagate registry assembly errors.
    ///
    /// # Deprecated
    ///
    /// Production assembly should call [`try_register`](Self::try_register).
    pub fn register(&self, registry: &mut ToolRegistry) {
        self.try_register(registry)
            .expect("flux-cognition pack registration failed");
    }
}

/// One single-shot text completion: stream and collect every [`Chunk::TextDelta`] plus the call's
/// [`Usage`] (mirrors `flux-flow`'s result-plus-usage stream collector). Usage stays outside the
/// call's [`Result`]: provider usage chunks are cumulative within one call, so the last one wins;
/// a provider that reports none yields a zero `Usage`. `usage_observation` is updated as chunks are
/// consumed so dropping this future can retain only the usage observed before cancellation.
async fn run_model(
    provider: &dyn Provider,
    model: &str,
    system: &str,
    prompt: &str,
    thinking: bool,
    effort: Option<Effort>,
    usage_observation: &mut UsageObservationGuard,
) -> (Result<String>, Usage) {
    let req = Request::new(model.to_string(), prompt.to_string())
        .with_system(system.to_string())
        .with_max_tokens(MAX_TOKENS)
        .with_thinking(thinking);
    let req = match effort {
        Some(effort) => req.with_effort(effort),
        None => req,
    };
    let mut stream = match provider.stream(req).await {
        Ok(stream) => stream,
        Err(error) => return (Err(error), Usage::default()),
    };
    let mut out = String::new();
    let mut usage = Usage::default();
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(Chunk::TextDelta(t)) => out.push_str(&t),
            Ok(Chunk::Usage(u)) => {
                usage = u;
                usage_observation.observe(&usage);
            }
            Ok(_) => {}
            Err(error) => return (Err(error), usage),
        }
    }
    (Ok(out), usage)
}

/// A required, non-optional param.
fn required(name: &str, ty: TypeRef) -> Param {
    Param {
        name: name.to_string(),
        ty,
        optional: false,
    }
}

/// An optional param (present in `properties`, absent from `required`).
fn optional(name: &str, ty: TypeRef) -> Param {
    Param {
        name: name.to_string(),
        ty,
        optional: true,
    }
}

/// Render a param as prompt text: a string stays as-is, anything else is serialized to compact
/// JSON, and a missing key is the empty string. Keeps `execute` robust to absent optional params.
fn field_text(params: &Value, key: &str) -> String {
    match params.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
        None => String::new(),
    }
}

/// Render the runtime's `Ctx` artifact for a reasoning prompt. New context packs carry a bounded
/// `content` payload plus audit metadata; only the purpose and payload belong in the model prompt.
/// A string or a legacy object without `content` keeps the old generic rendering for compatibility.
fn context_text(params: &Value) -> String {
    let Some(ctx) = params.get("ctx") else {
        return String::new();
    };
    let Some(fields) = ctx.as_object() else {
        return match ctx {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
    };
    let Some(content) = fields.get("content").and_then(Value::as_str) else {
        return ctx.to_string();
    };
    match fields.get("purpose").and_then(Value::as_str) {
        Some(purpose) if !purpose.trim().is_empty() => {
            format!("Purpose: {purpose}\n\n{content}")
        }
        _ => content.to_string(),
    }
}

/// Read a required input, erroring if it is missing or renders empty.
fn require(params: &Value, key: &str, op: &str) -> Result<String> {
    let text = field_text(params, key);
    if text.trim().is_empty() {
        return Err(Error::Other(format!(
            "{op}: required param `{key}` missing"
        )));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_provider::ChunkStream;
    use flux_spec::Effect;
    use flux_system::{System, Workspace};
    use serde_json::json;

    /// A hermetic provider that ignores the request and replays a single canned text delta.
    struct MockProvider {
        reply: String,
    }

    struct RequestCaptureProvider(std::sync::Mutex<Option<Request>>);

    #[async_trait]
    impl Provider for RequestCaptureProvider {
        fn name(&self) -> &str {
            "capture"
        }

        async fn stream(&self, req: Request) -> Result<ChunkStream> {
            *self.0.lock().unwrap() = Some(req);
            Ok(Box::pin(futures::stream::iter(vec![Ok(Chunk::TextDelta(
                "[]".into(),
            ))])))
        }
    }

    #[tokio::test]
    async fn cognition_requests_inherit_agent_reasoning_settings() {
        let provider = Arc::new(RequestCaptureProvider(std::sync::Mutex::new(None)));
        let mut reg = ToolRegistry::new();
        CognitionPack::new(provider.clone(), "test-model")
            .with_reasoning(true, Some(flux_provider::Effort::Medium))
            .register(&mut reg);
        reg.get("ai.extract")
            .unwrap()
            .execute(&ctx(), json!({ "from": "x" }))
            .await
            .unwrap();
        let req = provider.0.lock().unwrap().clone().unwrap();
        assert!(req.thinking);
        assert_eq!(req.effort, Some(flux_provider::Effort::Medium));
    }

    #[async_trait]
    impl Provider for MockProvider {
        fn name(&self) -> &str {
            "mock"
        }

        async fn stream(&self, _req: Request) -> Result<ChunkStream> {
            let chunks = vec![
                Ok(Chunk::TextDelta(self.reply.clone())),
                Ok(Chunk::Done { stop_reason: None }),
            ];
            Ok(Box::pin(futures::stream::iter(chunks)))
        }
    }

    fn pack(reply: &str) -> (CognitionPack, ToolRegistry) {
        let provider = Arc::new(MockProvider {
            reply: reply.to_string(),
        });
        let pack = CognitionPack::new(provider, "test-model");
        let mut reg = ToolRegistry::new();
        pack.register(&mut reg);
        (pack, reg)
    }

    /// A throwaway `ToolContext` rooted at a fresh temp dir (the cognition ops never touch it, but
    /// `execute` requires one).
    fn ctx() -> ToolContext {
        let dir = std::env::temp_dir().join(format!(
            "flux-cognition-test-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap())))
    }

    #[test]
    fn registers_all_six_ops() {
        let (_pack, reg) = pack("noop");
        let mut names = reg.names();
        names.sort();
        assert_eq!(
            names,
            vec![
                "ai.extract",
                "ai.judge",
                "ai.rank",
                "ai.reason",
                "ai.rewrite",
                "synth"
            ]
        );
    }

    #[test]
    fn each_spec_lowers_to_a_named_schema_with_model_facts() {
        let (_pack, reg) = pack("noop");
        for name in [
            "ai.extract",
            "ai.rank",
            "ai.judge",
            "ai.reason",
            "synth",
            "ai.rewrite",
        ] {
            let spec = reg.get(name).unwrap().spec();
            // A real object schema with named properties and a required array (P0 lowering).
            assert_eq!(spec.input_schema["type"], "object", "{name}");
            assert!(
                spec.input_schema["properties"].is_object(),
                "{name} properties"
            );
            assert!(spec.input_schema["required"].is_array(), "{name} required");
            assert!(
                !spec.input_schema["required"].as_array().unwrap().is_empty(),
                "{name} should have at least one required param"
            );
            // Host facts the envelope gates on: network egress + provider access, and — since
            // C-208 — `Risk::Medium`, because every op here spends money on a provider call and
            // `Risk::Low` would admit it to the pre-approval gather path.
            assert!(spec.has_effect(Effect::Network), "{name} effect");
            assert!(spec.access.contains(&AccessKind::Provider), "{name} access");
            assert_eq!(spec.risk, Risk::Medium, "{name} risk");
            assert!(
                flux_spec::metadata_violations(&spec, &[]).is_empty(),
                "{name} must satisfy the coherence invariants"
            );
        }
        // The tier reaches the registered `ToolSpec` from the typed contract, not from a second
        // declaration at the `Tool::spec` seam. Until C-208 those two disagreed — `opspec()` said
        // one thing and a trailing `.with_risk(…)` silently overrode it — which is precisely the
        // drift shape this invariant set exists to catch.
        for kind in OpKind::ALL {
            assert_eq!(
                kind.opspec().risk,
                reg.get(kind.name()).unwrap().spec().risk,
                "{} declares its risk in one place",
                kind.name()
            );
        }
    }

    #[test]
    fn extract_schema_has_typed_named_slots() {
        let (_pack, reg) = pack("noop");
        let spec = reg.get("ai.extract").unwrap().spec();
        let props = &spec.input_schema["properties"];
        assert_eq!(props["from"], json!({ "type": "string" }));
        assert_eq!(props["ask"], json!({ "type": "string" }));
        // Only the primary input `from` is required (matches what `execute` actually requires);
        // `ask`/`schema` are read leniently, so they are optional in the schema.
        assert_eq!(spec.input_schema["required"], json!(["from"]));
    }

    /// Every artifact-typed `#/$defs/<Name>` ref a cognition op declares must name a real prelude
    /// type — guards against the `Context`/`Verdict` drift class (a ref with no schema to resolve).
    #[test]
    fn cognition_op_refs_resolve_against_the_prelude() {
        let (_pack, reg) = pack("noop");
        let prelude: std::collections::HashSet<&str> =
            flux_lang::prelude::PRELUDE_TYPES.iter().copied().collect();
        for op in [
            "ai.extract",
            "ai.rank",
            "ai.judge",
            "ai.reason",
            "synth",
            "ai.rewrite",
        ] {
            let spec = reg.get(op).unwrap().spec();
            let s = serde_json::to_string(&spec.input_schema).unwrap();
            for frag in s.split("#/$defs/").skip(1) {
                let name: String = frag
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                assert!(
                    prelude.contains(name.as_str()),
                    "op `{op}` references unknown prelude type `{name}`"
                );
            }
        }
    }

    #[test]
    fn judge_output_slots_are_artifact_typed() {
        let (_pack, reg) = pack("noop");
        let spec = reg.get("ai.judge").unwrap().spec();
        let props = &spec.input_schema["properties"];
        // Artifact-typed slots lower to a `$ref` into `#/$defs/<Name>`.
        assert_eq!(props["claim"], json!({ "$ref": "#/$defs/Claim" }));
        assert_eq!(props["evidence"], json!({ "$ref": "#/$defs/Evidence" }));
    }

    /// `ctx` is a runtime value with audit metadata plus a materialized, budgeted payload. The
    /// reasoning prompt must pass the payload itself to the provider, not serialize the metadata
    /// (`name`, `members`, `budget`) as if those symbol names were the source documents.
    #[test]
    fn reason_prompt_uses_materialized_context_content() {
        let prompt = OpKind::Reason
            .prompt(&json!({
                "ask": "How long is recovery?",
                "ctx": {
                    "name": "handbook",
                    "purpose": "answer from the handbook",
                    "members": ["product", "policies"],
                    "budget": 5000,
                    "content": "## $product\nWorkspaces recover for 30 days."
                }
            }))
            .unwrap();
        assert!(prompt.contains("Workspaces recover for 30 days."));
        assert!(
            !prompt.contains("\"members\""),
            "metadata leaked into prompt: {prompt}"
        );
        assert!(
            !prompt.contains("\"budget\""),
            "metadata leaked into prompt: {prompt}"
        );
    }

    #[tokio::test]
    async fn extract_passes_mocked_json_through() {
        let reply = r#"[{"claim":"the sky is blue"}]"#;
        let (_pack, reg) = pack(reply);
        let out = reg
            .get("ai.extract")
            .unwrap()
            .execute(
                &ctx(),
                json!({ "from": "the sky is blue", "ask": "extract claims" }),
            )
            .await
            .unwrap();
        assert!(!out.is_error);
        assert_eq!(out.content, reply);
    }

    #[tokio::test]
    async fn synth_returns_mocked_answer() {
        let reply = r#"{"status":"ok","summary":"s","evidence":[],"gaps":[],"risks":[]}"#;
        let (_pack, reg) = pack(reply);
        let out = reg
            .get("synth")
            .unwrap()
            .execute(&ctx(), json!({ "claims": [{ "id": 1 }] }))
            .await
            .unwrap();
        assert_eq!(out.content, reply);
    }

    #[tokio::test]
    async fn rewrite_returns_mocked_text() {
        let (_pack, reg) = pack("HELLO WORLD");
        let out = reg
            .get("ai.rewrite")
            .unwrap()
            .execute(&ctx(), json!({ "text": "hello world", "style": "shouty" }))
            .await
            .unwrap();
        assert_eq!(out.content, "HELLO WORLD");
    }

    #[tokio::test]
    async fn missing_required_param_errors() {
        let (_pack, reg) = pack("noop");
        // `ai.judge` requires `claim`; omitting it is an error, not a silent empty prompt.
        let err = reg
            .get("ai.judge")
            .unwrap()
            .execute(&ctx(), json!({ "evidence": "some evidence" }))
            .await;
        assert!(err.is_err());
    }

    /// A provider that bills a fixed [`Usage`] — so a cognition op's model call has spend to record.
    struct UsageProvider(Usage);
    #[async_trait]
    impl Provider for UsageProvider {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(&self, _req: Request) -> Result<ChunkStream> {
            let chunks = vec![
                Ok(Chunk::TextDelta("[]".into())),
                Ok(Chunk::Usage(self.0.clone())),
                Ok(Chunk::Done { stop_reason: None }),
            ];
            Ok(Box::pin(futures::stream::iter(chunks)))
        }
    }

    /// A declared provider failure can arrive after the provider has already reported billable
    /// usage for the call. The error itself must remain the call's result; usage is an independent
    /// accounting outcome.
    struct UsageThenErrorProvider(Usage);

    #[async_trait]
    impl Provider for UsageThenErrorProvider {
        fn name(&self) -> &str {
            "mock"
        }

        async fn stream(&self, _req: Request) -> Result<ChunkStream> {
            Ok(Box::pin(futures::stream::iter(vec![
                Ok(Chunk::Usage(self.0.clone())),
                Err(Error::Provider("declared stream failure".into())),
            ])))
        }
    }

    /// A stream that reports one usage snapshot, then remains pending until its owning cognition
    /// future is cancelled/dropped. `pending` fires only after the usage chunk was consumed.
    struct UsageThenPendingStream {
        usage: Option<Usage>,
        pending: Arc<tokio::sync::Notify>,
    }

    impl futures::Stream for UsageThenPendingStream {
        type Item = Result<Chunk>;

        fn poll_next(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            if let Some(usage) = self.usage.take() {
                return std::task::Poll::Ready(Some(Ok(Chunk::Usage(usage))));
            }
            self.pending.notify_one();
            std::task::Poll::Pending
        }
    }

    struct UsageThenPendingProvider {
        usage: Usage,
        pending: Arc<tokio::sync::Notify>,
    }

    #[async_trait]
    impl Provider for UsageThenPendingProvider {
        fn name(&self) -> &str {
            "mock"
        }

        async fn stream(&self, _req: Request) -> Result<ChunkStream> {
            Ok(Box::pin(UsageThenPendingStream {
                usage: Some(self.usage.clone()),
                pending: self.pending.clone(),
            }))
        }
    }

    #[derive(Default)]
    struct UsageLoopHost(std::sync::Mutex<Vec<(String, String, Usage)>>);

    impl LoopHost for UsageLoopHost {
        fn record_model_usage(&self, provider: &str, model: &str, usage: Usage) {
            self.0
                .lock()
                .unwrap()
                .push((provider.to_string(), model.to_string(), usage));
        }
    }

    /// D-150: a cognition op's model call was billing tokens that got dropped on the floor. It now
    /// records a `cognition.usage` observation (op + model + the call's `Usage`) on the shared
    /// evidence log — the side-channel a `FlowClient` run reads to sum `ExecutionResult.usage`.
    #[tokio::test]
    async fn cognition_op_records_a_usage_observation_when_the_call_bills() {
        let usage = Usage {
            input_tokens: 100,
            output_tokens: 20,
            ..Default::default()
        };
        let mut reg = ToolRegistry::new();
        CognitionPack::new(Arc::new(UsageProvider(usage)), "test-model").register(&mut reg);
        let host = Arc::new(UsageLoopHost::default());
        let mut ctx = ctx();
        ctx.set_session("test-session");
        ctx.loop_host = Some(host.clone());
        reg.get("ai.extract")
            .unwrap()
            .execute(&ctx, json!({ "from": "x", "ask": "y" }))
            .await
            .unwrap();

        let log = ctx.evidence.lock().unwrap();
        let recorded: Vec<&Observation> = log.by_kind("cognition.usage").collect();
        assert_eq!(recorded.len(), 1, "one usage observation per model call");
        assert_eq!(recorded[0].data["op"], "ai.extract");
        assert_eq!(recorded[0].data["model"], "test-model");
        let u: Usage = serde_json::from_value(recorded[0].data["usage"].clone()).unwrap();
        assert_eq!(u.input_tokens, 100);
        assert_eq!(u.output_tokens, 20);
        assert_eq!(
            host.0.lock().unwrap().len(),
            1,
            "normal completion publishes turn usage once"
        );
    }

    /// C-66: usage observed before a declared provider error is still billable. It must be recorded
    /// exactly once without wrapping or replacing the provider's original error.
    #[tokio::test]
    async fn cognition_op_retains_usage_before_declared_provider_error() {
        let usage = Usage {
            input_tokens: 73,
            output_tokens: 11,
            ..Default::default()
        };
        let mut reg = ToolRegistry::new();
        CognitionPack::new(
            Arc::new(UsageThenErrorProvider(usage.clone())),
            "test-model",
        )
        .register(&mut reg);
        let host = Arc::new(UsageLoopHost::default());
        let mut ctx = ctx();
        ctx.set_session("test-session");
        ctx.loop_host = Some(host.clone());

        let error = reg
            .get("ai.extract")
            .unwrap()
            .execute(&ctx, json!({ "from": "x" }))
            .await
            .expect_err("the declared provider failure remains terminal");
        match error {
            Error::Provider(message) => assert_eq!(message, "declared stream failure"),
            other => panic!("expected the original provider error, got {other:?}"),
        }

        let recorded: Vec<_> = ctx
            .evidence
            .lock()
            .unwrap()
            .by_kind("cognition.usage")
            .cloned()
            .collect();
        assert_eq!(recorded.len(), 1, "failed call usage is recorded once");
        let recorded_usage: Usage =
            serde_json::from_value(recorded[0].data["usage"].clone()).unwrap();
        assert_eq!(recorded_usage, usage);
        assert_eq!(
            host.0.lock().unwrap().as_slice(),
            &[("mock".into(), "test-model".into(), usage)],
            "turn accounting sees the failed call exactly once"
        );
    }

    /// C-66: cancelling the task drops the in-flight cognition future. Drop-time accounting keeps
    /// the last usage snapshot that was actually observed, while Tokio still reports cancellation
    /// as the terminal state (rather than a fabricated provider success/error).
    #[tokio::test]
    async fn cognition_op_retains_observed_usage_when_cancelled_and_dropped() {
        let usage = Usage {
            input_tokens: 41,
            output_tokens: 3,
            ..Default::default()
        };
        let pending = Arc::new(tokio::sync::Notify::new());
        let mut reg = ToolRegistry::new();
        CognitionPack::new(
            Arc::new(UsageThenPendingProvider {
                usage: usage.clone(),
                pending: pending.clone(),
            }),
            "test-model",
        )
        .register(&mut reg);
        let tool = reg.get("ai.extract").unwrap();
        let host = Arc::new(UsageLoopHost::default());
        let mut ctx = ctx();
        ctx.set_session("test-session");
        ctx.loop_host = Some(host.clone());
        let run_ctx = ctx.clone();
        let task =
            tokio::spawn(async move { tool.execute(&run_ctx, json!({ "from": "x" })).await });

        pending.notified().await;
        task.abort();
        let terminal = task.await.expect_err("aborting drops the cognition future");
        assert!(
            terminal.is_cancelled(),
            "cancellation remains the terminal state: {terminal}"
        );

        let recorded: Vec<_> = ctx
            .evidence
            .lock()
            .unwrap()
            .by_kind("cognition.usage")
            .cloned()
            .collect();
        assert_eq!(recorded.len(), 1, "drop records observed usage once");
        let recorded_usage: Usage =
            serde_json::from_value(recorded[0].data["usage"].clone()).unwrap();
        assert_eq!(
            recorded_usage, usage,
            "only the observed snapshot is billed"
        );
        assert_eq!(
            host.0.lock().unwrap().as_slice(),
            &[("mock".into(), "test-model".into(), usage)],
            "drop publishes the observed call to turn accounting once"
        );
    }

    /// A free call (the `mock` provider reports no usage) records nothing — so a free run stays
    /// `ExecutionResult.usage: None` instead of a bogus zero-usage entry.
    #[tokio::test]
    async fn cognition_op_records_nothing_when_the_call_is_free() {
        let (_pack, reg) = pack("[]"); // MockProvider bills no usage
        let host = Arc::new(UsageLoopHost::default());
        let mut ctx = ctx();
        ctx.set_session("test-session");
        ctx.loop_host = Some(host.clone());
        reg.get("ai.extract")
            .unwrap()
            .execute(&ctx, json!({ "from": "x" }))
            .await
            .unwrap();
        assert_eq!(
            ctx.evidence
                .lock()
                .unwrap()
                .by_kind("cognition.usage")
                .count(),
            0,
            "a call the provider billed nothing for records no usage observation"
        );
        assert!(
            host.0.lock().unwrap().is_empty(),
            "zero usage must not create a turn-accounting call"
        );
    }
}
