//! Attach an async live system of record through the SDK and call its generated operations through
//! the real authorization → approval → guarded-execution envelope. The backend is hermetic, so the
//! example needs no API key or external service.
//!
//! Run with: `cargo run -p codewandler-flux-sdk --example live_datasource`

#[path = "support/live_datasource.rs"]
mod support;

use std::sync::Arc;

use async_trait::async_trait;
use flux_core::{Error, Result};
use flux_provider::{ChunkStream, Provider, Request};
use flux_sdk::datasource::LiveDatasource;
use flux_sdk::tools::ToolResult;
use flux_sdk::Client;
use serde_json::json;

use support::SupportBackend;

struct UnusedProvider;

#[async_trait]
impl Provider for UnusedProvider {
    fn name(&self) -> &str {
        "unused"
    }

    async fn stream(&self, _request: Request) -> Result<ChunkStream> {
        Err(Error::Other(
            "this example dispatches datasource operations without invoking a model".into(),
        ))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let backend = Arc::new(SupportBackend::new());
    let live_backend: Arc<dyn LiveDatasource> = backend.clone();
    let client = Client::builder()
        .model("unused")
        .auto_approve(true)
        .try_with_live_datasource("support", live_backend)?
        .build(Box::new(UnusedProvider), ".")?;

    let first = require_success(
        client
            .engine()
            .executor
            .dispatch(
                "support.list",
                json!({
                    "entity": "ticket",
                    "limit": 1,
                    "filters": {"state": "open", "priority": 2, "escalated": true}
                }),
            )
            .await,
    )?;
    println!("first page:\n{}", first.content);

    let cursor = first
        .content
        .lines()
        .find_map(|line| line.strip_prefix("next: "))
        .ok_or_else(|| Error::Other("first support page did not return a cursor".into()))?;
    let second = require_success(
        client
            .engine()
            .executor
            .dispatch(
                "support.list",
                json!({
                    "entity": "ticket",
                    "page": cursor,
                    "limit": 1,
                    "filters": {"state": "open", "priority": 2, "escalated": true}
                }),
            )
            .await,
    )?;
    println!("second page:\n{}", second.content);

    let ticket = require_success(
        client
            .engine()
            .executor
            .dispatch("support.get", json!({"entity": "ticket", "id": "T-100"}))
            .await,
    )?;
    println!("full ticket:\n{}", ticket.content);
    println!("backend calls: {}", backend.entries());
    Ok(())
}

fn require_success(result: ToolResult) -> Result<ToolResult> {
    if result.is_error {
        Err(Error::Other(result.content))
    } else {
        Ok(result)
    }
}
