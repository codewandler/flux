//! The **a2a** adapter (`kind = "a2a"`): expose a program agent over the full HTTP/A2A API — REST
//! sessions, SSE streaming, A2A JSON-RPC, and agent-card discovery. Unlike the event-source channels
//! (cron/webhook/slack, which deliver events into the bus), this channel talks **directly** to the
//! target agent's [`FlowEngine`], so conversational sessions and token streaming are preserved exactly.
//! It mounts [`flux_server::router`] (the one HTTP implementation) with graceful shutdown — this is the
//! surface that the removed `flux serve` command used to provide.

use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use flux_app::App;
use flux_flow::engine::FlowEngine;
use flux_lang::program::ChannelDecl;
use flux_server::CardInfo;

use crate::config::A2aSettings;
use crate::{Channel, Deliverer};

pub struct A2aChannel {
    name: String,
    addr: SocketAddr,
    token: Option<String>,
    engine: Arc<FlowEngine>,
    card: CardInfo,
}

impl A2aChannel {
    /// Build the channel from its declaration, resolving the target agent's engine from `app`. The
    /// engine must come from the live `App` (not the decl alone), so this is built by the host rather
    /// than the decl-only [`build_channels`](crate::build_channels).
    pub fn from_decl_and_app(decl: &ChannelDecl, app: &App) -> anyhow::Result<Self> {
        let s: A2aSettings = serde_json::from_value(decl.settings.clone())
            .map_err(|e| anyhow::anyhow!("channel `{}` settings: {e}", decl.name))?;
        let addr = SocketAddr::from_str(&s.addr)
            .map_err(|e| anyhow::anyhow!("channel `{}`: bad addr `{}`: {e}", decl.name, s.addr))?;
        // The served agent has no interactive approver, so an open non-loopback listener is a remote
        // surface — require a bearer token there, mirroring the webhook channel and flux-server.
        let token = s.token;
        if !addr.ip().is_loopback() && token.is_none() {
            anyhow::bail!(
                "channel `{}`: refusing to bind non-loopback {addr} without a `token` \
                 (set `token secret \"KEY\"`)",
                decl.name
            );
        }
        // Resolve the target agent: the explicit `agent` setting, else the program's sole agent.
        let agent_name = match s.agent {
            Some(a) => a,
            None => app.sole_agent().map(|a| a.name.clone()).ok_or_else(|| {
                anyhow::anyhow!(
                    "channel `{}`: set `agent = \"<name>\"` — the program declares {} agents, so the \
                     target is ambiguous",
                    decl.name,
                    app.program().agents.len()
                )
            })?,
        };
        let engine = app
            .agent_engine(&agent_name)
            .map_err(|e| anyhow::anyhow!("channel `{}`: {e}", decl.name))?;
        let description = app
            .agent_decl(&agent_name)
            .and_then(|d| d.description.clone());
        let card = CardInfo::for_agent(&agent_name, description);
        Ok(Self {
            name: decl.name.clone(),
            addr,
            token,
            engine,
            card,
        })
    }
}

#[async_trait]
impl Channel for A2aChannel {
    fn name(&self) -> &str {
        &self.name
    }

    async fn start(&self, _d: Arc<dyn Deliverer>, cancel: CancellationToken) -> anyhow::Result<()> {
        let listener = tokio::net::TcpListener::bind(self.addr)
            .await
            .map_err(|e| anyhow::anyhow!("channel `{}`: bind {}: {e}", self.name, self.addr))?;
        let bound = listener.local_addr().unwrap_or(self.addr);
        eprintln!(
            "channel `{}`: serving agent API on http://{bound}  (card: /.well-known/agent-card.json, \
             a2a: /a2a)",
            self.name
        );
        let router =
            flux_server::router(self.engine.clone(), self.token.clone(), self.card.clone());
        axum::serve(listener, router)
            .with_graceful_shutdown(async move { cancel.cancelled().await })
            .await
            .map_err(|e| anyhow::anyhow!("channel `{}`: serve: {e}", self.name))
    }
}
