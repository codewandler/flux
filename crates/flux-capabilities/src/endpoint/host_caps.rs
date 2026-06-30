//! [`EndpointBrokerHostCaps`] — the L5 bridge that services a consumer plugin's `endpoint.discover`
//! host capability against the cross-plugin [`EndpointBroker`] (D-26).
//!
//! It wraps a generic inner `Arc<dyn HostCapabilities>` (so it composes **over** `DatasourceHostCaps`
//! / `SystemHostCaps` — endpoint discovery, datasource records, and system IO all reachable through
//! one caps chain). The `endpoint.discover` command is gated by the consumer plugin's manifest
//! (`discover` capability, deny-by-default); every other command delegates to the inner caps.
//! Discovery returns **weak references only** — never a resolved endpoint or a secret. This mirrors
//! the [`DatasourceHostCaps`] precedent.
//!
//! [`DatasourceHostCaps`]: crate::DatasourceHostCaps

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use flux_plugin::HostCapabilities;

use super::EndpointBroker;

/// Host capabilities = a generic inner caps chain **plus** the `endpoint.discover` command backed by
/// the shared [`EndpointBroker`]. Built per consumer plugin (so the broker can skip fanning a query
/// back into the requester, and the manifest's `discover` grant gates the capability).
pub struct EndpointBrokerHostCaps {
    inner: Arc<dyn HostCapabilities>,
    broker: Arc<EndpointBroker>,
    /// The consumer plugin's name — passed as `requester` so the broker never fans back into it.
    consumer: String,
    /// Whether this consumer's manifest granted the `endpoint.discover` capability (deny-by-default).
    discover_granted: bool,
}

impl EndpointBrokerHostCaps {
    /// Wrap `inner` so `endpoint.discover` hits `broker` on behalf of `consumer`, gated by
    /// `discover_granted` (the consumer manifest's `capabilities.discover`).
    pub fn new(
        inner: Arc<dyn HostCapabilities>,
        broker: Arc<EndpointBroker>,
        consumer: impl Into<String>,
        discover_granted: bool,
    ) -> Self {
        Self {
            inner,
            broker,
            consumer: consumer.into(),
            discover_granted,
        }
    }
}

#[async_trait]
impl HostCapabilities for EndpointBrokerHostCaps {
    async fn handle(&self, command: &str, payload: &Value) -> std::result::Result<Value, String> {
        match command {
            "endpoint.discover" => {
                if !self.discover_granted {
                    return Err("endpoint.discover not granted".to_string());
                }
                let product = payload
                    .get("product")
                    .and_then(|v| v.as_str())
                    .ok_or("endpoint.discover: missing `product`")?;
                let query = payload.get("query").cloned().unwrap_or(Value::Null);
                let limit = payload
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize)
                    .unwrap_or(usize::MAX);
                // A consumer plugin may scope its discovery with structured `cluster`/`namespace`
                // fields (or `cluster=`/`namespace=` tokens in `query`, which the broker extracts).
                let cluster = payload
                    .get("cluster")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.trim().is_empty());
                let namespace = payload
                    .get("namespace")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.trim().is_empty());
                let candidates = self
                    .broker
                    .discover(
                        product,
                        &query,
                        cluster,
                        namespace,
                        limit,
                        Some(&self.consumer),
                    )
                    .await;
                Ok(json!({ "candidates": candidates }))
            }
            // Everything else (datasource / process / secret / http / endpoint) is the inner caps' job.
            other => self.inner.handle(other, payload).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoint::{EndpointBroker, EndpointRegistry, PluginRegistry, ProviderInvoker};
    use flux_secret::endpoint::{EndpointCandidate, EndpointRef};

    /// A fake invoker that always returns one candidate for the queried product, so the host-caps
    /// gate can be tested without driving a real provider subprocess.
    struct OneCandidate;

    #[async_trait]
    impl ProviderInvoker for OneCandidate {
        async fn discover(
            &self,
            _name: &str,
            product: &str,
            _query: &Value,
            _cluster: Option<&str>,
            _namespace: Option<&str>,
            _limit: usize,
        ) -> std::result::Result<Vec<EndpointCandidate>, String> {
            Ok(vec![EndpointCandidate {
                endpoint: EndpointRef::discovered("e1", "https://e1.internal", product),
                score: 1.0,
                reasons: vec![],
            }])
        }
    }

    /// A broker whose registry has one manifest-only provider for `product` (so `providers_for`
    /// matches), driven by the [`OneCandidate`] fake. The provider's host is an idle `cat` the fake
    /// never touches (killed on drop).
    async fn broker_with_provider(product: &str) -> Arc<EndpointBroker> {
        let system =
            flux_system::System::new(flux_system::Workspace::new(std::env::temp_dir()).unwrap());
        let host = flux_plugin::PluginHost::spawn(&system, "cat", &[])
            .await
            .expect("spawn idle test host");
        let registry = Arc::new(PluginRegistry::new());
        registry.register(
            "prov",
            crate::endpoint::ProviderEntry {
                manifest: Arc::new(flux_plugin::PluginManifest {
                    name: "prov".into(),
                    discovers: vec![product.into()],
                    ..Default::default()
                }),
                host: Arc::new(tokio::sync::Mutex::new(host)),
                caps: Arc::new(flux_plugin::DenyHostCaps),
            },
        );
        Arc::new(EndpointBroker::new(
            Arc::new(OneCandidate),
            registry,
            Arc::new(EndpointRegistry::new()),
        ))
    }

    /// Inner caps that deny every command — proves delegation reaches the inner chain.
    struct DenyInner;

    #[async_trait]
    impl HostCapabilities for DenyInner {
        async fn handle(
            &self,
            command: &str,
            _payload: &Value,
        ) -> std::result::Result<Value, String> {
            Err(format!("inner: `{command}` denied"))
        }
    }

    #[tokio::test]
    async fn discover_capability_gated() {
        let broker = broker_with_provider("prometheus").await;

        // Denied when the consumer manifest did not grant `discover`.
        let denied = EndpointBrokerHostCaps::new(
            Arc::new(DenyInner),
            broker.clone(),
            "consumer",
            /* discover_granted */ false,
        );
        let err = denied
            .handle("endpoint.discover", &json!({ "product": "prometheus" }))
            .await;
        assert!(err.is_err(), "ungranted discover must be refused: {err:?}");

        // Granted → returns candidates.
        let granted = EndpointBrokerHostCaps::new(
            Arc::new(DenyInner),
            broker,
            "consumer",
            /* discover_granted */ true,
        );
        let out = granted
            .handle("endpoint.discover", &json!({ "product": "prometheus" }))
            .await
            .unwrap();
        let cands = out["candidates"].as_array().expect("candidates array");
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0]["id"], "@endpoint/e1");

        // A non-discover command delegates to the (denying) inner caps.
        assert!(granted.handle("http.do", &json!({})).await.is_err());
    }
}
