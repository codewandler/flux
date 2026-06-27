//! Author a Flux-Lang flow in Rust with the embedded DSL, then run it through the `FlowClient`
//! lifecycle — no model, no API key. The DSL is the third front door (alongside the classic agent
//! `Client` and `FlowClient::compile`'s NL→AST): you build the AST directly, the engine runs it.
//!
//! Run with: `cargo run -p flux-sdk --example dsl_loops`
//!
//! It demonstrates two things:
//!   1. A *loop* that actually executes — `each $f in $files: read $f` — dispatched through the real
//!      safety envelope against a temp workspace.
//!   2. A control-flow *showcase* (match / fallback / timeout / budget / race) that is built and
//!      analyzed to prove the DSL produces an analyzer-clean AST, without needing a model at runtime.

use std::sync::Arc;

use async_trait::async_trait;
use flux_core::Result;
use flux_provider::{ChunkStream, Provider, Request};
use flux_sdk::dsl::*;
use flux_sdk::FlowClient;
use serde_json::json;

/// A provider that is never actually called: this example builds the AST directly (no `compile`) and
/// only dispatches built-in ops (`read`), so the model is never reached. It exists because
/// `FlowClient::build` requires *a* provider.
struct UnusedProvider;

#[async_trait]
impl Provider for UnusedProvider {
    fn name(&self) -> &str {
        "unused"
    }
    async fn stream(&self, _req: Request) -> Result<ChunkStream> {
        Ok(Box::pin(futures::stream::empty()))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // A hermetic workspace with a few files for the loop to read.
    let root = std::env::temp_dir().join(format!("flux-dsl-loops-{}", std::process::id()));
    std::fs::create_dir_all(&root).map_err(|e| flux_core::Error::Other(e.to_string()))?;
    for (name, body) in [
        ("one.txt", "alpha"),
        ("two.txt", "beta"),
        ("three.txt", "gamma"),
    ] {
        std::fs::write(root.join(name), body)
            .map_err(|e| flux_core::Error::Other(e.to_string()))?;
    }

    let client = FlowClient::builder()
        .model("unused")
        .auto_approve(true) // no human in the loop for an example
        .build(Arc::new(UnusedProvider), &root)?;

    // The cognition pack is wired alongside the built-ins (proof the registry is assembled).
    println!(
        "ops available: {} (incl. ai.extract, synth, …)",
        client.op_names().len()
    );

    // ---- 1. a loop that executes ---------------------------------------------------------------
    // each $f in ["one.txt", …] -> $contents: read $f   ;   return $contents
    // (the list is iterated as a literal directly — the runtime binds only the results of ops, not
    // bare literals, so there is no `$files = […]` statement.)
    let loop_flow = Flow::named("read_each")
        .body(|b| {
            b.each("f", lit(json!(["one.txt", "two.txt", "three.txt"])), |e| {
                e.collect("contents");
                e.body(|b| {
                    b.call("read", [var("f")]);
                });
            });
            b.ret(var("contents"));
        })
        .build();

    client
        .analyze(&loop_flow)
        .map_err(|d| flux_core::Error::Other(format!("analyze: {d:?}")))?;
    let out = client.execute(&loop_flow).await?;
    println!(
        "\n[loop] dispatched {} ops: {:?}",
        out.steps, out.tool_calls
    );
    println!("[loop] result: {}", out.result.replace('\n', " "));

    // ---- 2. a control-flow showcase (built + analyzed) -----------------------------------------
    // Every loop/guard primitive in one flow. We build and analyze it (the DSL's job is producing an
    // analyzer-clean AST); a real run of the model-routed/raced branches would need a live provider.
    let showcase = Flow::named("control_flow")
        .param("mode", TypeRef::String)
        .body(|b| {
            // match — exhaustive branch on a flow parameter.
            b.match_(var("mode"), |m| {
                m.case(lit("fast"), |b| {
                    b.call("read", [lit("one.txt")]);
                });
                m.default(|b| {
                    b.call("read", [lit("two.txt")]);
                });
            });
            // fallback — first branch that succeeds wins.
            b.fallback(|f| {
                f.bind("picked");
                f.branch(|b| {
                    b.call("read", [lit("missing.txt")]);
                });
                f.branch(|b| {
                    b.call("read", [lit("three.txt")]);
                });
            });
            // timeout + budget — reliability/cost guards wrapping a body.
            b.timeout(2000, |w| {
                w.bind("guarded");
                w.body(|b| {
                    b.budget(3, |w| {
                        w.body(|b| {
                            b.call("read", [lit("one.txt")]);
                        });
                    });
                });
            });
            b.ret(var("picked"));
        })
        .build();

    match client.analyze(&showcase) {
        Ok(()) => println!(
            "\n[showcase] built a {}-statement control-flow flow; analyze: clean ✓",
            showcase.body.len()
        ),
        Err(diags) => println!("\n[showcase] analyze rejected: {diags:?}"),
    }

    std::fs::remove_dir_all(&root).ok();
    Ok(())
}
