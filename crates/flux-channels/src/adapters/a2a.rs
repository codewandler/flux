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
use flux_system::System;

use crate::config::A2aSettings;
use crate::{Channel, ChannelContext, Deliverer};

pub struct A2aChannel {
    name: String,
    addr: SocketAddr,
    auth: flux_server::ServerAuth,
    engine: Arc<FlowEngine>,
    card: CardInfo,
}

impl A2aChannel {
    /// Build the channel from its declaration, resolving the target agent's engine from `app`. The
    /// engine must come from the live `App` (not the decl alone), so this is built by the host rather
    /// than the decl-only [`build_channels`](crate::build_channels).
    pub async fn from_decl_and_app(decl: &ChannelDecl, app: &App) -> anyhow::Result<Self> {
        let s: A2aSettings = serde_json::from_value(decl.settings.clone())
            .map_err(|e| anyhow::anyhow!("channel `{}` settings: {e}", decl.name))?;
        let addr = SocketAddr::from_str(&s.addr)
            .map_err(|e| anyhow::anyhow!("channel `{}`: bad addr `{}`: {e}", decl.name, s.addr))?;
        // Resolve the auth mode: `introspect_url` → per-request principal auth (D-69), else the
        // optional bearer token, else open. The served agent has no interactive approver, so an
        // open non-loopback listener is a remote surface — require authentication there, mirroring
        // the webhook channel and `flux --serve`.
        //
        // Matching the `Open` variant is sufficient *here* only because `a2a_auth_from_settings`
        // has already refused an empty `token`, which would otherwise reach this line as a
        // `SharedSecret` that is `Open` in effect (C-321). `flux_server::router` re-checks the
        // property rather than the variant, so this check being variant-shaped is a convenience,
        // not the guarantee.
        let auth = a2a_auth_from_settings(&s, &decl.name)?;
        if matches!(auth, flux_server::ServerAuth::Open) && !addr.ip().is_loopback() {
            anyhow::bail!(
                "channel `{}`: refusing to bind non-loopback {addr} without authentication \
                 (set `token secret \"KEY\"`, or `introspect_url` for per-request principal auth)",
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
            .await
            .map_err(|e| anyhow::anyhow!("channel `{}`: {e}", decl.name))?;
        let description = app
            .agent_decl(&agent_name)
            .and_then(|d| d.description.clone());
        let card = CardInfo::for_agent(&agent_name, description);
        Ok(Self {
            name: decl.name.clone(),
            addr,
            auth,
            engine,
            card,
        })
    }

    async fn serve_on(&self, context: ChannelContext) -> anyhow::Result<()> {
        let limits = flux_server::ServerLimits::from_env();
        let listener = flux_server::bind_http_listener(
            context.execution_system.as_ref(),
            self.addr,
            flux_server::bind_exposure(&self.auth),
            limits,
        )
        .await
        .map_err(|error| anyhow::anyhow!("channel `{}`: bind {}: {error}", self.name, self.addr))?;
        let bound = listener.local_addr().unwrap_or(self.addr);
        eprintln!(
            "channel `{}`: serving agent API on http://{bound}  (card: /.well-known/agent-card.json, \
             a2a: /a2a)",
            self.name
        );
        let router = flux_server::router(
            self.engine.clone(),
            self.auth.clone(),
            self.card.clone(),
            bound,
        )
        .map_err(|error| anyhow::anyhow!("channel `{}`: {error}", self.name))?;
        axum::serve(listener, router)
            .with_graceful_shutdown(async move { context.cancel.cancelled().await })
            .await
            .map_err(|error| anyhow::anyhow!("channel `{}`: serve: {error}", self.name))
    }
}

/// Map an `[a2a]` channel's settings onto a [`flux_server::ServerAuth`]. Principal mode is selected
/// by `introspect_url` and routed through the ONE construction point
/// ([`flux_server::PrincipalAuth::from_introspection`]) so the security-critical claim mapping is
/// identical to `flux --serve`. The client secret arrives already host-resolved (the program writes
/// `introspect_secret secret "KEY"`), so — unlike the CLI's env-var-NAME convention — it is a plain
/// value here by the time settings deserialize.
fn a2a_auth_from_settings(s: &A2aSettings, name: &str) -> anyhow::Result<flux_server::ServerAuth> {
    let Some(endpoint) = s.introspect_url.clone() else {
        // **An empty token is not a token, and it is worse than none** (C-321, the third instance of
        // the bypass D-216 and C-317 closed in the connector and webhook adapters).
        //
        // `token ""` — or `token secret "K"` where `K` is exported empty, because
        // `flux_app::resolve_secrets` resolves through `std::env::var`, which does not filter an
        // empty value — arrives here as `Some("")`. `ServerAuth::shared_secret` maps that to
        // `SharedSecret { secret: "" }`, *not* `Open`, so the `Open`-keyed non-loopback guard below
        // does not fire; and the server's bearer check compares the presented token (which is `""`
        // when the request carries no `Authorization` header at all) against the expected one, and
        // two empty byte strings are equal. The result is a public listener in front of a daemon
        // that auto-approves every tool call, presented to its operator as authenticated.
        //
        // Refused rather than normalised to `None`, and refused on **loopback** too: normalising is
        // silent, and would ship an operator a channel they believe is authenticated, one `addr`
        // edit away from being public. flux-server refuses the same configuration twice more
        // (`guard_open_bind` will not build a public router for it, `require_auth` will not
        // authenticate against it), so no half depends on another being right — but this is the
        // half that turns it into a config error the operator can act on, before a port is bound.
        if s.token.as_deref().is_some_and(|t| t.trim().is_empty()) {
            anyhow::bail!(
                "channel `{name}`: `token` is set but empty, which would authenticate every \
                 request — including one carrying no `Authorization` header at all. Give it a \
                 value, or remove it (a loopback bind needs none). A `secret \"KEY\"` reference \
                 resolves to an empty string when `KEY` is exported empty."
            );
        }
        // Shared-secret (or open) mode. Advertise `external_url` on the card when set, so a
        // non-loopback shared-secret channel isn't exposed to Host-poisoning of its card.
        return Ok(flux_server::ServerAuth::shared_secret(
            s.token.clone(),
            s.external_url.clone(),
        ));
    };
    let external_url = s.external_url.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "channel `{name}`: `external_url` is required with `introspect_url` — the public agent \
             card advertises where clients send bearer tokens, so it must come from config"
        )
    })?;
    let client = match (&s.introspect_client_id, &s.introspect_secret) {
        (Some(id), Some(secret)) => Some((id.clone(), secret.clone())),
        (Some(_), None) => anyhow::bail!(
            "channel `{name}`: `introspect_secret secret \"KEY\"` is required with introspect_client_id"
        ),
        (None, Some(_)) => anyhow::bail!(
            "channel `{name}`: `introspect_secret` is set without `introspect_client_id` — the \
             client secret would be silently ignored; set introspect_client_id or remove it"
        ),
        (None, None) => None,
    };
    if s.token.is_some() {
        eprintln!(
            "(channel `{name}`: `token` ignored — `introspect_url` enables per-request principal auth)"
        );
    }
    let auth = flux_server::PrincipalAuth::from_introspection(flux_server::IntrospectionParams {
        endpoint,
        client,
        allow_http: s.introspect_allow_http.unwrap_or(false),
        account_claim: s.introspect_account_claim.clone(),
        roles_claim: s.introspect_roles_claim.clone(),
        require_account: s.introspect_require_account.unwrap_or(false),
        external_url,
    })
    .map_err(|e| anyhow::anyhow!("channel `{name}`: introspection config: {e}"))?;
    Ok(flux_server::ServerAuth::Principal(auth))
}

#[async_trait]
impl Channel for A2aChannel {
    fn name(&self) -> &str {
        &self.name
    }

    async fn start(&self, _d: Arc<dyn Deliverer>, cancel: CancellationToken) -> anyhow::Result<()> {
        let system = Arc::new(System::from_env(std::env::current_dir()?)?);
        self.serve_on(ChannelContext {
            deliverer: _d,
            cancel,
            execution_system: system,
        })
        .await
    }

    async fn start_with_context(&self, context: ChannelContext) -> anyhow::Result<()> {
        self.serve_on(context).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(json: serde_json::Value) -> A2aSettings {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn no_introspect_is_token_or_open() {
        // Bare addr → open (loopback bind will be enforced separately).
        let open =
            a2a_auth_from_settings(&settings(serde_json::json!({ "addr": "127.0.0.1:0" })), "c")
                .unwrap();
        assert!(matches!(open, flux_server::ServerAuth::Open));
        // A token → shared secret.
        let tok = a2a_auth_from_settings(
            &settings(serde_json::json!({ "addr": "0.0.0.0:0", "token": "s3cr3t" })),
            "c",
        )
        .unwrap();
        assert!(matches!(tok, flux_server::ServerAuth::SharedSecret { .. }));
    }

    /// C-321, the producer half: `token ""` — or `token secret "K"` with `K` exported empty, which
    /// `flux_app::resolve_secrets` resolves through `std::env::var` without filtering — arrives here
    /// as `Some("")` and must be refused at load, before a port is bound. Refused on loopback too:
    /// normalising it to `Open` would be silent, and would ship an operator a channel they believe
    /// is authenticated, one `addr` edit away from being public.
    #[test]
    fn empty_token_is_refused_at_load() {
        for addr in ["127.0.0.1:0", "0.0.0.0:0"] {
            for token in ["", "   "] {
                let err = a2a_auth_from_settings(
                    &settings(serde_json::json!({ "addr": addr, "token": token })),
                    "c",
                )
                .unwrap_err()
                .to_string();
                assert!(
                    err.contains("empty"),
                    "`token = {token:?}` on {addr} must be refused as empty; got: {err}"
                );
            }
        }
    }

    #[test]
    fn introspect_url_selects_principal_mode() {
        let auth = a2a_auth_from_settings(
            &settings(serde_json::json!({
                "addr": "0.0.0.0:0",
                "introspect_url": "https://idp.example/introspect",
                "external_url": "https://agent.example.com",
                "introspect_account_claim": "org_id",
            })),
            "c",
        )
        .unwrap();
        assert!(matches!(auth, flux_server::ServerAuth::Principal(_)));
    }

    #[test]
    fn introspect_url_requires_external_url() {
        let err = a2a_auth_from_settings(
            &settings(serde_json::json!({
                "addr": "0.0.0.0:0",
                "introspect_url": "https://idp.example/introspect",
            })),
            "c",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("external_url"), "got: {err}");
    }

    #[test]
    fn introspect_client_id_requires_secret() {
        let err = a2a_auth_from_settings(
            &settings(serde_json::json!({
                "addr": "0.0.0.0:0",
                "introspect_url": "https://idp.example/introspect",
                "external_url": "https://agent.example.com",
                "introspect_client_id": "flux-server",
            })),
            "c",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("introspect_secret"), "got: {err}");
    }

    #[test]
    fn http_endpoint_rejected_without_allow_http() {
        let err = a2a_auth_from_settings(
            &settings(serde_json::json!({
                "addr": "0.0.0.0:0",
                "introspect_url": "http://idp.internal/introspect",
                "external_url": "https://agent.example.com",
            })),
            "c",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("introspection config"), "got: {err}");
    }
}
