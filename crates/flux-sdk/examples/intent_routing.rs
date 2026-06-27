//! Intent detection → routing, authored with the Rust DSL and run through `FlowClient`. This is the
//! DSL-native form of `crates/flux-lang/examples/call-routing.flux`: a single "model-cost" step
//! classifies the caller's utterance into an intent, then everything else is **deterministic routing**.
//!
//! The model/datasource adapters are mocked — registered stub ops — so it runs with no API key.
//! `route` is the *bounded non-determinism* primitive: the (mock) model picks **which** declared branch
//! runs, never **what** it does.
//!
//! Run with: `cargo run -p flux-sdk --example intent_routing`

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
/// response. Stands in for a model-backed or datasource-backed op.
struct MockOp {
    name: &'static str,
    param: &'static str,
    respond: fn(&str) -> String,
}

#[async_trait]
impl Tool for MockOp {
    fn spec(&self) -> ToolSpec {
        // The input schema's `required` names drive positional-arg binding: a call's first arg lands in
        // `params[self.param]`.
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

/// The single model-cost step: classify an utterance into a **bare intent label** (what `route`'s
/// selector matches on). A real build would back this with `ai.reason`/a classifier.
fn classify(utterance: &str) -> String {
    let u = utterance.to_lowercase();
    if u.contains("cancel") {
        "cancel_booking"
    } else if u.contains("change") || u.contains("reschedule") || u.contains("move") {
        "change_booking"
    } else if u.contains("book") || u.contains("flight") {
        "book_flight"
    } else {
        "escalate"
    }
    .to_string()
}

fn adapters() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(MockOp {
            name: "intent.classify",
            param: "utterance",
            respond: classify,
        }),
        Arc::new(MockOp {
            name: "booking.create",
            param: "utterance",
            respond: |_| "Booking confirmed. Reference FX-1234, departing soon.".to_string(),
        }),
        Arc::new(MockOp {
            name: "booking.modify",
            param: "utterance",
            respond: |_| "Your booking has been updated.".to_string(),
        }),
        Arc::new(MockOp {
            name: "booking.cancel",
            param: "utterance",
            respond: |_| "Your booking has been cancelled.".to_string(),
        }),
        Arc::new(MockOp {
            name: "support.ticket",
            param: "utterance",
            respond: |_| "Connecting you to an agent. Ticket SUP-77.".to_string(),
        }),
    ]
}

/// `route( intent.classify(utterance) ) { … }` — the classifier's label selects one fixed, deterministic
/// branch. Each branch's handler op produces the caller-facing response.
fn route_call(utterance: &str) -> DraftAst {
    Flow::named("route_call")
        .body(|b| {
            b.route(call("intent.classify", [lit(utterance)]), |r| {
                r.case("book_flight", |b| {
                    b.call("booking.create", [lit(utterance)]);
                });
                r.case("change_booking", |b| {
                    b.call("booking.modify", [lit(utterance)]);
                });
                r.case("cancel_booking", |b| {
                    b.call("booking.cancel", [lit(utterance)]);
                });
                // baggage_enquiry, escalate, or anything the classifier couldn't place
                r.default(|b| {
                    b.call("support.ticket", [lit(utterance)]);
                });
            });
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

    for utterance in [
        "I'd like to book a flight to Berlin next Tuesday",
        "can you change my booking to Friday?",
        "please cancel my booking, reference FX-9",
        "where do I collect my baggage?",
    ] {
        let flow = route_call(utterance);
        client
            .analyze(&flow)
            .map_err(|d| Error::Other(format!("analyze: {d:?}")))?;
        let out = client.execute(&flow).await?;
        println!("\nutterance: {utterance}");
        println!("  ops: {:?}", out.tool_calls);
        println!("  response: {}", out.result.replace('\n', " "));
    }
    Ok(())
}
