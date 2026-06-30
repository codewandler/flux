//! The cross-plugin endpoint-discovery **fan-out broker** (D-26, L5).
//!
//! A consumer plugin asks the host *"which endpoints exist for product X?"* (the `endpoint.discover`
//! host capability). The host never lets plugins address each other: the [`EndpointBroker`] is the
//! only intermediary. It matches the product against every registered provider whose manifest
//! `discovers` it, calls each provider's `endpoint.discover` op, aggregates and ranks the candidates
//! by score, commits them to the shared [`EndpointRegistry`], and returns **weak references only** —
//! never a [`ResolvedEndpoint`](flux_secret::endpoint::ResolvedEndpoint), never a secret value.
//!
//! Provider `endpoint.discover` ops are read-only by contract: this is a discovery/query path, so the
//! broker only ever calls the `endpoint.discover` op — never an effectful one.
//!
//! Layering: this is L5 (it owns the registry), driving L4 plugin hosts (`flux-plugin`) through the
//! same guarded [`HostCapabilities`] the plugin's tools run under — the [`DatasourceHostCaps`]
//! precedent.
//!
//! [`DatasourceHostCaps`]: crate::DatasourceHostCaps

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use flux_plugin::{HostCapabilities, PluginHost, PluginManifest};
use flux_secret::endpoint::{EndpointCandidate, EndpointRecord};

use super::EndpointRegistry;

/// A registered provider plugin: the handles the broker needs to fan a discovery query out to it.
pub struct ProviderEntry {
    /// The provider's manifest (its `discovers` set is matched against the queried product).
    pub manifest: Arc<PluginManifest>,
    /// The shared subprocess connection (driven behind a mutex like the plugin's own tools).
    pub host: Arc<Mutex<PluginHost>>,
    /// The guarded host capabilities the provider's ops — including `endpoint.discover` — run under.
    pub caps: Arc<dyn HostCapabilities>,
}

/// A session-scoped registry of loaded plugins, keyed by plugin name. The surface registers each
/// loaded plugin here so the broker can look up providers by the products they `discovers`.
#[derive(Default)]
pub struct PluginRegistry {
    entries: RwLock<HashMap<String, ProviderEntry>>,
}

impl PluginRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
        }
    }

    /// Register (or replace) a loaded plugin under `name`.
    pub fn register(&self, name: impl Into<String>, entry: ProviderEntry) {
        self.entries.write().unwrap().insert(name.into(), entry);
    }

    /// The host + caps + manifest for `name`, if registered. Returns clones of the `Arc` handles so
    /// the caller does not hold the registry lock across an `await`.
    pub fn get(&self, name: &str) -> Option<ProviderEntry> {
        self.entries
            .read()
            .unwrap()
            .get(name)
            .map(|e| ProviderEntry {
                manifest: e.manifest.clone(),
                host: e.host.clone(),
                caps: e.caps.clone(),
            })
    }

    /// Names of every registered provider that can discover `product` (its manifest `discovers` it),
    /// sorted for a stable fan-out order.
    pub fn providers_for(&self, product: &str) -> Vec<String> {
        let guard = self.entries.read().unwrap();
        let mut names: Vec<String> = guard
            .iter()
            .filter(|(_, e)| e.manifest.discovers.iter().any(|p| p == product))
            .map(|(name, _)| name.clone())
            .collect();
        names.sort();
        names
    }
}

/// The seam the broker calls to ask one provider to discover endpoints for a product. Abstracted so
/// the broker is unit-testable without spawning subprocesses (the production impl drives a real
/// plugin host; tests use a fake).
#[async_trait]
pub trait ProviderInvoker: Send + Sync {
    /// Invoke provider `name`'s `endpoint.discover` op for `product`, returning its candidates. A
    /// transport/provider error is returned as `Err` (the broker logs + skips it, never fatal).
    async fn discover(
        &self,
        name: &str,
        product: &str,
        query: &Value,
        limit: usize,
    ) -> Result<Vec<EndpointCandidate>, String>;
}

/// The production [`ProviderInvoker`]: looks the provider up in the [`PluginRegistry`] and drives its
/// real plugin host's `endpoint.discover` op under the provider's own guarded capabilities.
pub struct HostProviderInvoker {
    registry: Arc<PluginRegistry>,
}

impl HostProviderInvoker {
    /// Drive providers registered in `registry`.
    pub fn new(registry: Arc<PluginRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl ProviderInvoker for HostProviderInvoker {
    async fn discover(
        &self,
        name: &str,
        product: &str,
        query: &Value,
        limit: usize,
    ) -> Result<Vec<EndpointCandidate>, String> {
        let entry = self
            .registry
            .get(name)
            .ok_or_else(|| format!("no such provider `{name}`"))?;
        let payload = json!({ "product": product, "query": query, "limit": limit });
        let result = {
            let mut host = entry.host.lock().await;
            host.call_with_host("endpoint.discover", payload, entry.caps.as_ref())
                .await
                .map_err(|e| e.to_string())?
        };
        let candidates: Vec<EndpointCandidate> =
            serde_json::from_value(result.get("candidates").cloned().unwrap_or(Value::Null))
                .map_err(|e| {
                    format!("provider `{name}` endpoint.discover: bad `candidates`: {e}")
                })?;
        Ok(candidates)
    }
}

/// The cross-plugin discovery broker: fans a product query out to its providers, ranks the union, and
/// commits the result to the [`EndpointRegistry`] — returning weak refs only.
pub struct EndpointBroker {
    invoker: Arc<dyn ProviderInvoker>,
    registry: Arc<PluginRegistry>,
    endpoints: Arc<EndpointRegistry>,
    /// Providers currently being invoked on this task. The **re-entrancy guard**: a consumer plugin's
    /// `endpoint.discover` callback runs *while that plugin's host mutex is already locked* on this
    /// task, so fanning back into that same provider would deadlock. We never invoke a provider that
    /// is in flight (and the broker also skips the `requester` by name).
    in_flight: Mutex<HashSet<String>>,
}

impl EndpointBroker {
    /// A broker over `invoker`, the `registry` of loaded plugins, and the shared endpoint `registry`.
    pub fn new(
        invoker: Arc<dyn ProviderInvoker>,
        registry: Arc<PluginRegistry>,
        endpoints: Arc<EndpointRegistry>,
    ) -> Self {
        Self {
            invoker,
            registry,
            endpoints,
            in_flight: Mutex::new(HashSet::new()),
        }
    }

    /// Discover endpoints for `product`: fan out to every matching provider (except `requester` and
    /// any already in flight), aggregate, rank by `score` descending, truncate to `limit`, commit each
    /// to the endpoint registry (owner = the discovering provider), and return the **weak** candidates.
    ///
    /// A provider error is logged and skipped — one bad provider never fails the query. The result is
    /// references only: no `ResolvedEndpoint`, no secret value ever crosses this boundary.
    pub async fn discover(
        &self,
        product: &str,
        query: &Value,
        limit: usize,
        requester: Option<&str>,
    ) -> Vec<EndpointCandidate> {
        let providers = self.registry.providers_for(product);
        // Keep each candidate paired with the provider that returned it, so the committed record's
        // `owner` is the discovering provider — `replace_owned`-able on a later refresh.
        let mut found: Vec<(String, EndpointCandidate)> = Vec::new();

        for name in providers {
            if Some(name.as_str()) == requester {
                continue; // never fan a plugin's query back into itself
            }
            // Re-entrancy guard: skip + claim atomically so a re-entrant call can't slip past.
            {
                let mut guard = self.in_flight.lock().await;
                if guard.contains(&name) {
                    continue;
                }
                guard.insert(name.clone());
            }
            let outcome = self.invoker.discover(&name, product, query, limit).await;
            self.in_flight.lock().await.remove(&name);

            match outcome {
                Ok(cands) => found.extend(cands.into_iter().map(|c| (name.clone(), c))),
                Err(e) => eprintln!("(provider `{name}` endpoint.discover failed: {e})"),
            }
        }

        // Stable sort by score descending (ties keep provider fan-out order), then truncate.
        found.sort_by(|a, b| {
            b.1.score
                .partial_cmp(&a.1.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        found.truncate(limit);

        // Commit each to the registry as a discovered record owned by its discovering provider, so a
        // later reference resolves to the same weak record (still no secret — only the credential ref).
        for (owner, cand) in &found {
            self.endpoints.put(EndpointRecord {
                owner: owner.clone(),
                ..EndpointRecord::config(cand.endpoint.clone())
            });
        }

        found.into_iter().map(|(_, cand)| cand).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_secret::endpoint::EndpointRef;
    use flux_secret::Ref;

    /// A fake invoker mapping `(provider name -> candidates)`, so the broker can be exercised without
    /// spawning subprocesses.
    struct FakeInvoker {
        by_provider: HashMap<String, Vec<EndpointCandidate>>,
    }

    #[async_trait]
    impl ProviderInvoker for FakeInvoker {
        async fn discover(
            &self,
            name: &str,
            _product: &str,
            _query: &Value,
            _limit: usize,
        ) -> Result<Vec<EndpointCandidate>, String> {
            Ok(self.by_provider.get(name).cloned().unwrap_or_default())
        }
    }

    fn provider_manifest(name: &str, discovers: &[&str]) -> Arc<PluginManifest> {
        Arc::new(PluginManifest {
            name: name.to_string(),
            discovers: discovers.iter().map(|s| s.to_string()).collect(),
            ..PluginManifest::default()
        })
    }

    /// Register provider `name` (discovering `product`) in `reg`. The fake [`ProviderInvoker`] never
    /// touches the host/caps, so a throwaway idle `cat` subprocess satisfies `ProviderEntry`'s host
    /// field (killed on drop); only the manifest's `discovers` set is ever read (`providers_for`).
    async fn register_provider(reg: &PluginRegistry, name: &str, product: &str) {
        let system =
            flux_system::System::new(flux_system::Workspace::new(std::env::temp_dir()).unwrap());
        let host = PluginHost::spawn(&system, "cat", &[])
            .await
            .expect("spawn idle test host");
        reg.register(
            name,
            ProviderEntry {
                manifest: provider_manifest(name, &[product]),
                host: Arc::new(Mutex::new(host)),
                caps: Arc::new(flux_plugin::DenyHostCaps),
            },
        );
    }

    fn candidate(id: &str, product: &str, score: f64, provider: &str) -> EndpointCandidate {
        EndpointCandidate {
            endpoint: EndpointRef::discovered(id, format!("https://{id}.internal"), product),
            score,
            reasons: vec![format!("provider:{provider}")],
        }
    }

    #[tokio::test]
    async fn broker_fans_out_and_ranks() {
        let reg = Arc::new(PluginRegistry::new());
        register_provider(&reg, "a", "prometheus").await;
        register_provider(&reg, "b", "prometheus").await;

        let mut by_provider = HashMap::new();
        by_provider.insert(
            "a".to_string(),
            vec![candidate("pa", "prometheus", 0.4, "a")],
        );
        by_provider.insert(
            "b".to_string(),
            vec![
                candidate("pb1", "prometheus", 0.9, "b"),
                candidate("pb2", "prometheus", 0.6, "b"),
            ],
        );
        let invoker = Arc::new(FakeInvoker { by_provider });
        let endpoints = Arc::new(EndpointRegistry::new());
        let broker = EndpointBroker::new(invoker, reg.clone(), endpoints.clone());

        // Union of both providers, sorted by score descending.
        let all = broker.discover("prometheus", &json!({}), 10, None).await;
        let ids: Vec<&str> = all.iter().map(|c| c.endpoint.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["@endpoint/pb1", "@endpoint/pb2", "@endpoint/pa"],
            "union ranked by score desc"
        );

        // Each candidate is committed to the registry, owned by its discovering provider.
        let rec = endpoints
            .resolve("@endpoint/pb1")
            .expect("committed record");
        assert_eq!(rec.owner, "b");
        assert_eq!(endpoints.resolve("@endpoint/pa").unwrap().owner, "a");

        // Truncation to `limit` keeps the top-scoring candidates.
        let top = broker.discover("prometheus", &json!({}), 1, None).await;
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].endpoint.id, "@endpoint/pb1");
    }

    #[tokio::test]
    async fn discover_skips_the_requester() {
        let reg = Arc::new(PluginRegistry::new());
        register_provider(&reg, "self", "prometheus").await;
        register_provider(&reg, "other", "prometheus").await;

        let mut by_provider = HashMap::new();
        by_provider.insert(
            "self".to_string(),
            vec![candidate("ps", "prometheus", 0.9, "self")],
        );
        by_provider.insert(
            "other".to_string(),
            vec![candidate("po", "prometheus", 0.5, "other")],
        );
        let invoker = Arc::new(FakeInvoker { by_provider });
        let broker = EndpointBroker::new(invoker, reg, Arc::new(EndpointRegistry::new()));

        // The requesting plugin is never fanned back into itself.
        let found = broker
            .discover("prometheus", &json!({}), 10, Some("self"))
            .await;
        let ids: Vec<&str> = found.iter().map(|c| c.endpoint.id.as_str()).collect();
        assert_eq!(ids, vec!["@endpoint/po"]);
    }

    #[tokio::test]
    async fn discovery_results_carry_no_secrets() {
        // A discovery result is a list of weak `EndpointCandidate`s. A candidate may carry a
        // `credential_ref` (a *location*), never a value — assert the serialized form has the ref but
        // no secret material, and that the type cannot serialize a `ResolvedEndpoint`.
        let reg = Arc::new(PluginRegistry::new());
        register_provider(&reg, "k8s", "postgres").await;
        let cand = EndpointCandidate {
            endpoint: EndpointRef {
                credential_ref: Some(Ref::kubernetes("monitoring", "pg-creds", "password")),
                ..EndpointRef::discovered("pg-1", "postgres://db:5432/app", "postgres")
            },
            score: 1.0,
            reasons: vec!["provider:k8s".to_string()],
        };
        let mut by_provider = HashMap::new();
        by_provider.insert("k8s".to_string(), vec![cand]);
        let invoker = Arc::new(FakeInvoker { by_provider });
        let broker = EndpointBroker::new(invoker, reg, Arc::new(EndpointRegistry::new()));

        let found = broker.discover("postgres", &json!({}), 10, None).await;
        let serialized = serde_json::to_string(&found).unwrap();
        // The credential is present only as a reference (a location), never a value.
        assert!(
            serialized.contains("kubernetes")
                && serialized.contains("pg-creds")
                && serialized.contains("\"slot\":\"password\""),
            "candidate carries the credential *reference*: {serialized}"
        );
        // No injected/resolved auth material leaks into the discovery surface.
        assert!(!serialized.contains("injected_headers"));
        assert!(!serialized.to_lowercase().contains("bearer "));
    }
}
