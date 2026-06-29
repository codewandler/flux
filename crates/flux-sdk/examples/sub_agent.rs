//! Sub-agents through `FlowClient::with_sub_agents` — the WS1 consumption seam, hermetic (no API key).
//!
//! A parent flow delegates a bounded task to a named **role** (`scout`) via the `task` tool; the
//! sub-agent runs its own agent loop through the same safety envelope and returns its result. The
//! whole thing is wired with one call — `client.with_sub_agents(...)` — instead of hand-assembling the
//! spawner, executor, and tool context. Roles are registered **in memory** (no shared `.flux/agents`
//! dir), the model is mocked, so it runs offline.
//!
//! Run with: `cargo run -p flux-sdk --example sub_agent`

use std::sync::Arc;

use async_trait::async_trait;
use flux_core::{Chunk, ContentBlock, Result, StopReason};
use flux_orchestrate::{Role, RoleRegistry, SubAgents};
use flux_provider::{ChunkStream, Provider, Request};
use flux_runtime::ToolRegistry;
use flux_sdk::dsl::*;
use flux_sdk::FlowClient;

/// A canned provider: every turn returns one fixed text reply, then ends. Stands in for the model a
/// real sub-agent would drive — so the example needs no API key.
struct CannedProvider;

#[async_trait]
impl Provider for CannedProvider {
    fn name(&self) -> &str {
        "mock"
    }
    async fn stream(&self, _req: Request) -> Result<ChunkStream> {
        let chunks = vec![
            Chunk::Block(ContentBlock::Text {
                text: "scouted: 3 modules, no obvious issues".into(),
            }),
            Chunk::Done {
                stop_reason: Some(StopReason::EndTurn),
            },
        ];
        Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // A role registered in memory (the multi-tenant path — no shared `.flux/agents` directory).
    let roles = RoleRegistry::from_roles([Role {
        name: "scout".into(),
        description: "read-only reconnaissance".into(),
        model: None,             // inherit the spawner's default model
        tools: Some(Vec::new()), // a leaf with no tools — it just investigates and reports
        prompt: "You are a scout. Investigate and report findings tersely.".into(),
    }]);

    // The tool surface children may be granted (empty here; a real consumer subsets its own ops).
    // A fresh provider is built per sub-agent via the factory.
    let child_base = ToolRegistry::new();
    let factory = Arc::new(|| Ok(Box::new(CannedProvider) as Box<dyn Provider>));
    let sub_agents = SubAgents::new(roles, child_base, factory, "mock", 1024);

    // Build a client and attach the sub-agents in one call: this registers the `task` tool into the
    // client's catalog and installs the spawner into every run's context.
    let mut client = FlowClient::builder()
        .model("mock")
        .auto_approve(true)
        .build(Arc::new(CannedProvider), ".")?;
    client.with_sub_agents(sub_agents);

    // A parent flow that delegates to the scout. Positional args bind to the `task` schema's required
    // params in order: role, then task.
    let parent = flow(|b| {
        b.call(
            "task",
            [lit("scout"), lit("Look at the repo and summarize it.")],
        );
    });

    if let Err(diags) = client.analyze(&parent) {
        return Err(flux_core::Error::Other(format!("analyze: {diags:?}")));
    }
    let out = client.execute(&parent).await?;

    println!("scout returned: {}", out.result);
    assert!(
        out.result.contains("3 modules"),
        "the sub-agent's result should flow back to the parent (got: {:?})",
        out.result
    );
    println!("ops dispatched: {:?}", out.tool_calls);
    Ok(())
}
