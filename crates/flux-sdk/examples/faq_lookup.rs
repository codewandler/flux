//! FAQ / QA knowledge lookup, authored with the Rust DSL and run through `FlowClient`. A question is
//! looked up in a (mock) knowledge base; on a miss the flow **degrades gracefully** via `fallback`; a
//! (mock) synth adapter wraps the outcome into the typed prelude `Answer` artifact.
//!
//! All adapters are mocked — registered stub ops — so it runs with no API key.
//!
//! Run with: `cargo run -p flux-sdk --example faq_lookup`

use std::sync::Arc;

use async_trait::async_trait;
use flux_core::{Error, Result};
use flux_provider::{ChunkStream, Provider, Request};
use flux_runtime::{Tool, ToolContext, ToolResult};
use flux_sdk::dsl::*;
use flux_sdk::FlowClient;
use flux_spec::ToolSpec;
use serde_json::{json, Value};

/// A never-called provider — the flow builds its AST directly and dispatches only registered ops.
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

/// A deterministic stub op (the "adapter"): one string input parameter, a pure `&str -> String`
/// response. Stands in for a retrieval datasource or a model-backed synth op.
struct MockOp {
    name: &'static str,
    param: &'static str,
    respond: fn(&str) -> String,
}

#[async_trait]
impl Tool for MockOp {
    fn spec(&self) -> ToolSpec {
        let mut props = serde_json::Map::new();
        props.insert(self.param.to_string(), json!({ "type": "string" }));
        ToolSpec::read_only(
            self.name,
            "mock adapter (deterministic)",
            json!({ "type": "object", "properties": props, "required": [self.param] }),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let arg = params
            .get(self.param)
            .and_then(|v| v.as_str())
            .unwrap_or("");
        Ok(ToolResult::ok((self.respond)(arg)))
    }
}

/// The knowledge base: a keyword lookup. Returns the answer text, or `""` on a miss (so `fallback`
/// passes over it to the escalation branch).
fn kb_search(question: &str) -> String {
    let q = question.to_lowercase();
    if q.contains("password") {
        "Reset your password from Account → Security → Reset password.".to_string()
    } else if q.contains("refund") {
        "Refunds are processed back to the original payment method within 5–7 business days."
            .to_string()
    } else if q.contains("hours") || q.contains("open") {
        "Support is available 09:00–18:00 CET, Monday to Friday.".to_string()
    } else {
        String::new()
    }
}

/// A sentinel the escalation branch returns; the synth op keys its `unanswered` status off it.
const ESCALATE: &str = "ESCALATE";

fn adapters() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(MockOp {
            name: "kb.search",
            param: "question",
            respond: kb_search,
        }),
        Arc::new(MockOp {
            name: "kb.escalate",
            param: "question",
            respond: |_| format!("{ESCALATE}: no FAQ match — routed to a human (ticket FAQ-001)."),
        }),
        // The synth adapter: wrap whatever retrieval produced into a typed `Answer` JSON. `answered`
        // when it's a real KB hit; `unanswered` (+ a gap) when it's the escalation sentinel.
        Arc::new(MockOp {
            name: "faq.synth",
            param: "retrieved",
            respond: synth_answer,
        }),
    ]
}

fn synth_answer(retrieved: &str) -> String {
    let answer = if retrieved.starts_with(ESCALATE) {
        json!({
            "status": "unanswered",
            "summary": "No FAQ entry matched; escalated to a human agent.",
            "evidence": [],
            "gaps": ["question not covered by the FAQ knowledge base"],
            "risks": [],
        })
    } else {
        json!({
            "status": "answered",
            "summary": retrieved,
            "evidence": [],
            "gaps": [],
            "risks": [],
        })
    };
    serde_json::to_string(&answer).expect("Answer serializes")
}

/// fallback( kb.search else kb.escalate ) -> $kb ; $answer = faq.synth($kb) ; return $answer
fn faq_lookup(question: &str) -> DraftAst {
    Flow::named("faq_lookup")
        .body(|b| {
            // Retrieval with graceful degradation: first non-empty branch wins.
            b.fallback(|f| {
                f.bind("kb");
                f.branch(|b| {
                    b.call("kb.search", [lit(question)]);
                });
                f.branch(|b| {
                    b.call("kb.escalate", [lit(question)]);
                });
            });
            // Wrap the retrieved text into a typed Answer.
            b.bind("answer", call("faq.synth", [var("kb")]));
            b.ret(var("answer"));
        })
        .build()
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut client = FlowClient::builder()
        .model("mock")
        .auto_approve(true)
        .build(Arc::new(StubProvider), ".")?;
    for op in adapters() {
        client.register_op(op);
    }

    for question in [
        "How do I reset my password?",
        "What is your refund policy?",
        "Do you ship to Antarctica?", // a miss → escalation
    ] {
        let flow = faq_lookup(question);
        client
            .analyze(&flow)
            .map_err(|d| Error::Other(format!("analyze: {d:?}")))?;
        let out = client.execute(&flow).await?;
        println!("\nQ: {question}");
        println!("  ops: {:?}", out.tool_calls);
        // The flow returns an `Answer`-shaped JSON, so `ExecutionResult::answer()` round-trips it.
        match out.answer() {
            Some(a) => {
                println!("  [{}] {}", a.status, a.summary);
                if !a.gaps.is_empty() {
                    println!("  gaps: {:?}", a.gaps);
                }
            }
            None => println!("  result: {}", out.result),
        }
    }
    Ok(())
}
