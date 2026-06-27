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
//! op declares [`AccessKind::Provider`] and [`Risk::Low`].
//!
//! The pack is L3 because it depends on the L1 [`Provider`] abstraction. It is *provider-injected*:
//! construct [`CognitionPack::new`] with whatever provider/model the host has wired up and call
//! [`CognitionPack::register`] to add every op to a [`ToolRegistry`].

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::Value;

use flux_core::{Chunk, Error, Result};
use flux_lang::ast::{FlowEffect, TypeRef};
use flux_lang::opspec::{OpSpec, Param};
use flux_provider::{Provider, Request};
use flux_runtime::{Tool, ToolContext, ToolRegistry, ToolResult};
use flux_spec::{AccessKind, Idempotency, Risk, ToolSpec};

/// Token budget for a single cognition completion. Generous enough for a synthesized answer or an
/// extracted array, bounded so a runaway generation can't burn the whole context.
const MAX_TOKENS: u32 = 4096;

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
            risk: Risk::Low,
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
                let ctx = p("ctx");
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
}

#[async_trait]
impl Tool for CognitionOp {
    fn spec(&self) -> ToolSpec {
        // P0: lower the typed, named OpSpec to a faithful JSON-Schema ToolSpec, then layer on the
        // host facts the envelope gates on — a model call needs provider access (network egress).
        self.kind
            .opspec()
            .lower()
            .with_access(vec![AccessKind::Provider])
            .with_risk(Risk::Low)
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let prompt = self.kind.prompt(&params)?;
        let out = run_model(
            self.provider.as_ref(),
            &self.model,
            self.kind.system(),
            &prompt,
        )
        .await?;
        Ok(ToolResult::ok(out))
    }
}

/// A provider-injected pack of model-backed cognition ops. Construct it with the provider/model the
/// host has configured, then [`register`](Self::register) every op into a [`ToolRegistry`].
pub struct CognitionPack {
    provider: Arc<dyn Provider>,
    model: String,
}

impl CognitionPack {
    /// Build a pack bound to `provider` and a concrete `model` id (already resolved from any alias).
    pub fn new(provider: Arc<dyn Provider>, model: impl Into<String>) -> Self {
        Self {
            provider,
            model: model.into(),
        }
    }

    /// Register every cognition op (`ai.extract`, `ai.rank`, `ai.judge`, `ai.reason`, `synth`,
    /// `ai.rewrite`) into `registry`. Each op shares this pack's provider and model.
    pub fn register(&self, registry: &mut ToolRegistry) {
        for kind in OpKind::ALL {
            registry.register(Arc::new(CognitionOp {
                kind,
                provider: self.provider.clone(),
                model: self.model.clone(),
            }));
        }
    }
}

/// One single-shot text completion: stream and collect every [`Chunk::TextDelta`] (mirrors the
/// `run_model` helper in `flux-flow`'s compiler).
async fn run_model(
    provider: &dyn Provider,
    model: &str,
    system: &str,
    prompt: &str,
) -> Result<String> {
    let req = Request::new(model.to_string(), prompt.to_string())
        .with_system(system.to_string())
        .with_max_tokens(MAX_TOKENS);
    let mut stream = provider.stream(req).await?;
    let mut out = String::new();
    while let Some(chunk) = stream.next().await {
        if let Chunk::TextDelta(t) = chunk? {
            out.push_str(&t);
        }
    }
    Ok(out)
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
            // Host facts the envelope gates on: network egress + provider access, low risk.
            assert!(spec.has_effect(Effect::Network), "{name} effect");
            assert!(spec.access.contains(&AccessKind::Provider), "{name} access");
            assert_eq!(spec.risk, Risk::Low, "{name} risk");
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
}
