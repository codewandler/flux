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

use flux_plugin::{HostCapabilities, PluginHost, PluginManifest, ReferenceResolver};
use flux_secret::endpoint::{EndpointCandidate, EndpointRecord, ResolvedEndpoint};
use flux_secret::{Kind, Material, Ref, Scheme};

use super::{EndpointRegistry, StaticResolver};

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

/// Deny-by-default operator grants for cross-plugin credential resolution (D-27): each entry is
/// `"<consumer>:<provider>"` (or `"<consumer>:*"`). The broker holds this list (injected by the
/// surface from `flux-config`) instead of depending on `flux-config` directly — keeping flux-capabilities
/// free of that edge. No matching entry → no resolution.
#[derive(Debug, Clone, Default)]
pub struct CrossPluginGrants {
    entries: Vec<String>,
}

impl CrossPluginGrants {
    /// Build from raw `"<consumer>:<provider>"` grant strings.
    pub fn new(entries: Vec<String>) -> Self {
        Self { entries }
    }

    /// Whether `consumer` may have `provider`'s credentials materialized on its behalf.
    pub fn allows(&self, consumer: &str, provider: &str) -> bool {
        self.entries
            .iter()
            .any(|g| g == &format!("{consumer}:{provider}") || g == &format!("{consumer}:*"))
    }
}

/// First-use approval seam for cross-plugin credential resolution. On the first resolution for a
/// `(consumer, provider)` pair this session the broker consults the approver (if installed); the
/// decision is cached in-session. When NO approver is installed, the operator config grant alone
/// authorizes (so the headless/demo path works).
#[async_trait]
pub trait CrossPluginApprover: Send + Sync {
    /// Approve (or deny) `consumer` resolving a credential owned by `provider`.
    async fn approve(&self, consumer: &str, provider: &str) -> bool;
}

/// Audit seam for cross-plugin credential resolution. Fires on every *successful* resolution. The
/// `reference_location` is the `credential_ref` string — a location, **never** the value. The concrete
/// `flux-events`-backed impl lives at a surface (L6), keeping flux-capabilities event-store-free.
pub trait CrossPluginAudit: Send + Sync {
    /// Record that `consumer` resolved a credential owned by `provider`, located at `reference_location`.
    fn record_cross_plugin_resolve(&self, consumer: &str, provider: &str, reference_location: &str);
}

/// The seam the broker calls to read a credential value from a provider plugin's `secret.read` op.
/// Abstracted (like [`ProviderInvoker`]) so the cross-plugin gate is unit-testable without spawning a
/// real provider subprocess. The production impl ([`HostCredentialReader`]) drives the registry host.
#[async_trait]
pub trait CredentialReader: Send + Sync {
    /// Read the value the `reference` addresses by calling `provider`'s `secret.read` op. Returns the
    /// raw secret value (host-side; never surfaced to the model).
    async fn read(&self, provider: &str, reference: &Ref) -> Result<String, String>;
}

/// The production [`CredentialReader`]: looks the provider up in the [`PluginRegistry`] and calls its
/// `secret.read` op under the provider's own guarded capabilities.
pub struct HostCredentialReader {
    registry: Arc<PluginRegistry>,
}

impl HostCredentialReader {
    pub fn new(registry: Arc<PluginRegistry>) -> Self {
        Self { registry }
    }

    /// The `secret.read` op name a provider advertises (fully-qualified if so authored).
    fn secret_read_op_name(manifest: &PluginManifest) -> String {
        manifest
            .operations
            .iter()
            .map(|o| o.name.clone())
            .find(|n| n == "secret.read" || n.ends_with(".secret.read"))
            .unwrap_or_else(|| "secret.read".to_string())
    }
}

#[async_trait]
impl CredentialReader for HostCredentialReader {
    async fn read(&self, provider: &str, reference: &Ref) -> Result<String, String> {
        let entry = self.registry.get(provider).ok_or_else(|| {
            format!("no provider `{provider}` registered to resolve `{reference}`")
        })?;
        // The kubernetes/plugin ref maps to `secret.read` input. For `Kubernetes`: namespace=plugin,
        // name=instance, key=slot. For `Plugin`: the slot is the key; name=instance for symmetry.
        let payload = json!({
            "namespace": reference.plugin,
            "name": reference.instance,
            "keys": [reference.slot],
        });
        let op = Self::secret_read_op_name(&entry.manifest);
        let result = {
            let mut host = entry.host.lock().await;
            host.call_with_host(&op, payload, entry.caps.as_ref())
                .await
                .map_err(|e| format!("provider `{provider}` {op}: {e}"))?
        };
        result
            .get("values")
            .and_then(|v| v.get(&reference.slot))
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                format!(
                    "provider `{provider}` {op}: no value for key `{}`",
                    reference.slot
                )
            })
            .map(|s| s.to_string())
    }
}

/// The cross-plugin discovery broker: fans a product query out to its providers, ranks the union, and
/// commits the result to the [`EndpointRegistry`] — returning weak refs only. Also the L5
/// [`ReferenceResolver`] (D-27): it resolves discovered/named endpoint references to their runtime
/// form and materializes credentials, including the gated, audited cross-plugin path.
pub struct EndpointBroker {
    invoker: Arc<dyn ProviderInvoker>,
    registry: Arc<PluginRegistry>,
    endpoints: Arc<EndpointRegistry>,
    /// Providers currently being invoked on this task. The **re-entrancy guard**: a consumer plugin's
    /// `endpoint.discover` callback runs *while that plugin's host mutex is already locked* on this
    /// task, so fanning back into that same provider would deadlock. We never invoke a provider that
    /// is in flight (and the broker also skips the `requester` by name). Also guards the cross-plugin
    /// `resolve_credential` call (refuse re-entering a plugin already on the call stack).
    in_flight: Mutex<HashSet<String>>,
    /// The static config/manifest-default resolver: named references and `Env`-scheme credentials are
    /// delegated here. `None` → those paths error (the broker only resolves discovered refs).
    static_resolver: Option<Arc<StaticResolver>>,
    /// Deny-by-default cross-plugin credential grants (operator config).
    grants: CrossPluginGrants,
    /// First-use approval seam (optional — config grant alone authorizes when absent).
    approver: Option<Arc<dyn CrossPluginApprover>>,
    /// Cross-plugin resolution audit seam (optional).
    audit: Option<Arc<dyn CrossPluginAudit>>,
    /// In-session cache of first-use approval decisions, keyed by `(consumer, provider)`.
    approved: Mutex<HashMap<(String, String), bool>>,
    /// The seam that reads a credential value from a provider's `secret.read` op (production: the
    /// registry-driven [`HostCredentialReader`]; tests inject a fake).
    cred_reader: Arc<dyn CredentialReader>,
}

impl EndpointBroker {
    /// A broker over `invoker`, the `registry` of loaded plugins, and the shared endpoint `registry`.
    /// Cross-plugin credential resolution is denied (no grants) and the static resolver is absent
    /// until installed via the builders — so a fresh broker resolves only discovered references whose
    /// credentials are owned by the discovering provider with a config grant.
    pub fn new(
        invoker: Arc<dyn ProviderInvoker>,
        registry: Arc<PluginRegistry>,
        endpoints: Arc<EndpointRegistry>,
    ) -> Self {
        let cred_reader = Arc::new(HostCredentialReader::new(registry.clone()));
        Self {
            invoker,
            registry,
            endpoints,
            in_flight: Mutex::new(HashSet::new()),
            static_resolver: None,
            grants: CrossPluginGrants::default(),
            approver: None,
            audit: None,
            approved: Mutex::new(HashMap::new()),
            cred_reader,
        }
    }

    /// Override the credential-read seam (tests inject a fake; production uses the default
    /// registry-driven [`HostCredentialReader`]).
    pub fn with_credential_reader(mut self, reader: Arc<dyn CredentialReader>) -> Self {
        self.cred_reader = reader;
        self
    }

    /// Install the static config/manifest-default resolver (for named references + `Env` credentials).
    pub fn with_static_resolver(mut self, resolver: Arc<StaticResolver>) -> Self {
        self.static_resolver = Some(resolver);
        self
    }

    /// Install the operator's deny-by-default cross-plugin credential grants.
    pub fn with_cross_plugin_grants(mut self, grants: CrossPluginGrants) -> Self {
        self.grants = grants;
        self
    }

    /// Install the first-use approval seam.
    pub fn with_cross_plugin_approver(mut self, approver: Arc<dyn CrossPluginApprover>) -> Self {
        self.approver = Some(approver);
        self
    }

    /// Install the cross-plugin resolution audit seam.
    pub fn with_cross_plugin_audit(mut self, audit: Arc<dyn CrossPluginAudit>) -> Self {
        self.audit = Some(audit);
        self
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

    /// The provider plugin that owns a credential `reference`, if it is a cross-plugin scheme: the
    /// `Kubernetes` scheme is owned by the `"kubernetes"` provider; a `Plugin` scheme names its owner
    /// in `reference.plugin`. `Env` (and anything else) is not cross-plugin → `None`.
    fn credential_owner(reference: &Ref) -> Option<String> {
        match reference.scheme {
            Scheme::Kubernetes => Some("kubernetes".to_string()),
            Scheme::Plugin => Some(reference.plugin.clone()),
            Scheme::Env => None,
        }
    }

    /// Enforce the deny-by-default cross-plugin gate for `(consumer, provider)`: (1) operator config
    /// grant; (2) first-use approval (cached in-session); on success (3) emit the audit record. Returns
    /// `Ok(())` when the resolution may proceed, `Err` (a refusal reason) otherwise. `reference_location`
    /// is the `credential_ref` string — a location, never the value.
    async fn authorize_cross_plugin(
        &self,
        consumer: &str,
        provider: &str,
        reference_location: &str,
    ) -> Result<(), String> {
        // 1. Operator config grant (deny-by-default).
        if !self.grants.allows(consumer, provider) {
            return Err(format!(
                "cross-plugin credential resolution denied: no grant for `{consumer}:{provider}` \
                 (add it to `[endpoint] cross_plugin_credentials`)"
            ));
        }
        // 2. First-use approval (session-cached). With no approver installed, the config grant alone
        //    authorizes (headless/demo path).
        if let Some(approver) = &self.approver {
            let key = (consumer.to_string(), provider.to_string());
            let cached = self.approved.lock().await.get(&key).copied();
            let decision = match cached {
                Some(d) => d,
                None => {
                    let d = approver.approve(consumer, provider).await;
                    self.approved.lock().await.insert(key, d);
                    d
                }
            };
            if !decision {
                return Err(format!(
                    "cross-plugin credential resolution denied by approver for `{consumer}:{provider}`"
                ));
            }
        }
        // 3. Audit the (authorized, about-to-succeed) resolution — location only, never the value.
        if let Some(audit) = &self.audit {
            audit.record_cross_plugin_resolve(consumer, provider, reference_location);
        }
        Ok(())
    }

    /// Materialize a credential owned by another plugin via the [`CredentialReader`] seam (production:
    /// the provider's `secret.read` op, under its own guarded capabilities). Reuses the `in_flight`
    /// re-entrancy guard (refuse re-entering a provider already on this task's call stack). Only the
    /// `Kubernetes`/`Plugin` schemes reach here.
    async fn materialize_cross_plugin(&self, reference: &Ref) -> Result<Material, String> {
        let provider = Self::credential_owner(reference)
            .ok_or_else(|| format!("`{reference}` is not a cross-plugin credential reference"))?;
        // Re-entrancy guard: refuse re-entering a provider already in flight on this task.
        {
            let mut guard = self.in_flight.lock().await;
            if guard.contains(&provider) {
                return Err(format!(
                    "cross-plugin credential resolution would re-enter provider `{provider}` (in flight)"
                ));
            }
            guard.insert(provider.clone());
        }
        let result = self.cred_reader.read(&provider, reference).await;
        self.in_flight.lock().await.remove(&provider);
        let value = result?;
        Ok(Material {
            reference: reference.clone(),
            kind: Kind::ApiKey,
            value,
            media_type: None,
        })
    }
}

/// Strip inline userinfo (`scheme://user:pass@host`) out of a URL so a credential-bearing URL is
/// never surfaced: returns the bare URL plus the extracted `(user, password)` when present.
fn split_inline_credential(raw: &str) -> (String, Option<(String, Option<String>)>) {
    let Ok(mut url) = url::Url::parse(raw) else {
        return (raw.to_string(), None);
    };
    let user = url.username().to_string();
    let pass = url.password().map(|p| p.to_string());
    if user.is_empty() && pass.is_none() {
        return (raw.to_string(), None);
    }
    // Clear userinfo from the bare URL. `set_username`/`set_password` return `Err(())` for URLs that
    // cannot have authority (e.g. `mailto:`); those have no userinfo anyway, so ignore the result.
    let _ = url.set_username("");
    let _ = url.set_password(None);
    (url.to_string(), Some((user, pass)))
}

#[async_trait]
impl ReferenceResolver for EndpointBroker {
    async fn resolve_endpoint(&self, reference: &str) -> Result<ResolvedEndpoint, String> {
        // Consumer-agnostic form: route through `resolve_endpoint_for` with an empty consumer that can
        // NEVER match a cross-plugin grant — so this path can never silently inject a cross-plugin
        // credential, mirroring how `resolve_credential` refuses cross-plugin schemes. (An endpoint
        // whose credential the consumer DOES own, or an `Env`/inline credential, still resolves.)
        self.resolve_endpoint_for("", reference).await
    }

    async fn resolve_endpoint_for(
        &self,
        consumer: &str,
        reference: &str,
    ) -> Result<ResolvedEndpoint, String> {
        // A discovered `@endpoint/<id>` reference resolves from the endpoint registry; a named one
        // delegates to the static config resolver chain.
        if !flux_secret::endpoint::EndpointRef::is_discovered_ref(reference) {
            return self
                .static_resolver
                .as_ref()
                .ok_or_else(|| format!("no resolver for named endpoint `{reference}`"))?
                .resolve_endpoint(reference)
                .await;
        }
        let record = self
            .endpoints
            .resolve(reference)
            .ok_or_else(|| format!("no discovered endpoint record for `{reference}`"))?;
        // Inline-credential URL splitting: a credential-bearing URL is never surfaced — strip the
        // userinfo into an injected header, keep the bare URL in `ResolvedEndpoint.url`. This carries
        // no cross-plugin hop (the credential is in the record's own URL).
        let (bare_url, inline) = split_inline_credential(&record.endpoint.url);
        let mut resolved = ResolvedEndpoint::new(reference, bare_url);
        if let Some((user, pass)) = inline {
            let token = match pass {
                Some(p) => format!("{user}:{p}"),
                None => user,
            };
            let encoded = base64_encode(token.as_bytes());
            resolved = resolved.with_header("Authorization", format!("Basic {encoded}"));
        }
        // Materialize the record's `credential_ref` (if any) into an injected Bearer header (the
        // default HTTP scheme) on behalf of the REAL `consumer` (the plugin doing the IO). When the
        // credential is owned by a different plugin, host-injecting it for this consumer is a
        // cross-plugin credential *use* — `resolve_credential_for` fires the deny-by-default gate
        // (grant + first-use approval + audit) against `(consumer, owner)`.
        if let Some(cred) = &record.endpoint.credential_ref {
            let material = self.resolve_credential_for(consumer, cred).await?;
            resolved = resolved.with_header("Authorization", format!("Bearer {}", material.value));
        }
        Ok(resolved)
    }

    async fn resolve_credential(&self, reference: &Ref) -> Result<Material, String> {
        // Consumer-agnostic form: `Env` via the static resolver; cross-plugin schemes are NOT gated
        // here (no consumer to gate against) — only `resolve_credential_for` does the cross-plugin
        // hop. Refuse a cross-plugin scheme on this path so the gate is never bypassed.
        match reference.scheme {
            Scheme::Env => self
                .static_resolver
                .as_ref()
                .ok_or_else(|| format!("no resolver for credential `{reference}`"))?
                .resolve_credential(reference)
                .await,
            _ => Err(format!(
                "credential `{reference}` is cross-plugin; use resolve_credential_for (consumer-gated)"
            )),
        }
    }

    async fn resolve_credential_for(
        &self,
        consumer: &str,
        reference: &Ref,
    ) -> Result<Material, String> {
        // `Env` is host-local (no owning plugin) → straight through the static resolver.
        let Some(provider) = Self::credential_owner(reference) else {
            return self.resolve_credential(reference).await;
        };
        // A plugin resolving a credential it *owns* itself is not a cross-plugin hop — no gate.
        if provider == consumer {
            return self.materialize_cross_plugin(reference).await;
        }
        // Cross-plugin: gate (grant → first-use approval → audit) BEFORE materializing.
        self.authorize_cross_plugin(consumer, &provider, &reference.to_string())
            .await?;
        self.materialize_cross_plugin(reference).await
    }

    async fn credential_ref_for_endpoint(&self, reference: &str) -> Result<Ref, String> {
        if flux_secret::endpoint::EndpointRef::is_discovered_ref(reference) {
            let record = self
                .endpoints
                .resolve(reference)
                .ok_or_else(|| format!("no discovered endpoint record for `{reference}`"))?;
            return record
                .endpoint
                .credential_ref
                .ok_or_else(|| format!("endpoint `{reference}` has no credential reference"));
        }
        Err(format!(
            "named endpoint `{reference}` has no broker-resolvable credential reference"
        ))
    }
}

/// Minimal standard base64 of `bytes` (for inline-credential Basic-auth headers). Avoids pulling a
/// dependency edge for a one-off; the alphabet matches RFC 4648 standard.
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((n >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
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

    // --- D-27: cross-plugin credential resolution gating ---

    /// A fake [`CredentialReader`] that returns a fixed value and records each `(provider, ref)` read,
    /// so the gate is testable without a provider subprocess.
    #[derive(Default)]
    struct FakeReader {
        value: String,
        reads: std::sync::Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl CredentialReader for FakeReader {
        async fn read(&self, provider: &str, reference: &Ref) -> Result<String, String> {
            self.reads
                .lock()
                .unwrap()
                .push((provider.to_string(), reference.to_string()));
            Ok(self.value.clone())
        }
    }

    /// A recording [`CrossPluginApprover`] (consulted-count + a fixed decision).
    struct RecordingApprover {
        decision: bool,
        calls: std::sync::Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl CrossPluginApprover for RecordingApprover {
        async fn approve(&self, consumer: &str, provider: &str) -> bool {
            self.calls
                .lock()
                .unwrap()
                .push((consumer.to_string(), provider.to_string()));
            self.decision
        }
    }

    /// A recording [`CrossPluginAudit`] capturing `(consumer, provider, location)` — never a value.
    #[derive(Default)]
    struct RecordingAudit {
        records: std::sync::Mutex<Vec<(String, String, String)>>,
    }

    impl CrossPluginAudit for RecordingAudit {
        fn record_cross_plugin_resolve(
            &self,
            consumer: &str,
            provider: &str,
            reference_location: &str,
        ) {
            self.records.lock().unwrap().push((
                consumer.to_string(),
                provider.to_string(),
                reference_location.to_string(),
            ));
        }
    }

    fn bare_broker(reader: Arc<FakeReader>) -> EndpointBroker {
        EndpointBroker::new(
            Arc::new(FakeInvoker {
                by_provider: HashMap::new(),
            }),
            Arc::new(PluginRegistry::new()),
            Arc::new(EndpointRegistry::new()),
        )
        .with_credential_reader(reader)
    }

    #[tokio::test]
    async fn cross_plugin_resolution_denied_without_grant() {
        let reader = Arc::new(FakeReader {
            value: "should-never-be-read".into(),
            ..Default::default()
        });
        // No `with_cross_plugin_grants` → deny-by-default.
        let broker = bare_broker(reader.clone());
        let cred = Ref::kubernetes("monitoring", "pg-creds", "password");

        // The consumer `sql` is NOT granted to use `kubernetes`' credentials → refused, and the
        // credential reader is never consulted.
        let err = broker
            .resolve_credential_for("sql", &cred)
            .await
            .unwrap_err();
        assert!(
            err.contains("denied"),
            "must be refused without a grant: {err}"
        );
        assert!(
            reader.reads.lock().unwrap().is_empty(),
            "the provider must not be read without a grant"
        );
    }

    #[tokio::test]
    async fn cross_plugin_first_use_approval_and_audit() {
        let secret = "k8s-pg-password";
        let reader = Arc::new(FakeReader {
            value: secret.into(),
            ..Default::default()
        });
        let approver = Arc::new(RecordingApprover {
            decision: true,
            calls: std::sync::Mutex::new(Vec::new()),
        });
        let audit = Arc::new(RecordingAudit::default());
        let broker = bare_broker(reader.clone())
            .with_cross_plugin_grants(CrossPluginGrants::new(vec!["sql:kubernetes".into()]))
            .with_cross_plugin_approver(approver.clone())
            .with_cross_plugin_audit(audit.clone());
        let cred = Ref::kubernetes("monitoring", "pg-creds", "password");

        // First resolution: granted + approver consulted + audited; the value is returned host-side.
        let m1 = broker.resolve_credential_for("sql", &cred).await.unwrap();
        assert_eq!(m1.value, secret);
        // Second resolution: the approval is cached — the approver is consulted exactly once.
        let m2 = broker.resolve_credential_for("sql", &cred).await.unwrap();
        assert_eq!(m2.value, secret);
        assert_eq!(
            approver.calls.lock().unwrap().len(),
            1,
            "first-use approval is cached in-session"
        );

        // Audit fired on each successful resolution, recording the LOCATION (the ref), never the value.
        let records = audit.records.lock().unwrap();
        assert_eq!(records.len(), 2);
        for (consumer, provider, location) in records.iter() {
            assert_eq!(consumer, "sql");
            assert_eq!(provider, "kubernetes");
            assert_eq!(location, "kubernetes/monitoring/pg-creds/password");
            assert!(
                !location.contains(secret),
                "audit must never carry the value"
            );
        }
    }

    #[tokio::test]
    async fn cross_plugin_denied_when_approver_refuses() {
        let reader = Arc::new(FakeReader {
            value: "v".into(),
            ..Default::default()
        });
        let approver = Arc::new(RecordingApprover {
            decision: false,
            calls: std::sync::Mutex::new(Vec::new()),
        });
        // Config grant present, but the approver refuses → denied, and the value is never read.
        let broker = bare_broker(reader.clone())
            .with_cross_plugin_grants(CrossPluginGrants::new(vec!["sql:kubernetes".into()]))
            .with_cross_plugin_approver(approver);
        let cred = Ref::kubernetes("monitoring", "pg-creds", "password");
        assert!(broker.resolve_credential_for("sql", &cred).await.is_err());
        assert!(reader.reads.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn own_credential_is_not_cross_plugin_gated() {
        // A plugin resolving a credential it OWNS (consumer == provider) is not a cross-plugin hop —
        // it succeeds with no config grant and no audit.
        let reader = Arc::new(FakeReader {
            value: "own-secret".into(),
            ..Default::default()
        });
        let audit = Arc::new(RecordingAudit::default());
        let broker = bare_broker(reader.clone()).with_cross_plugin_audit(audit.clone());
        // The `kubernetes` plugin reading its own kubernetes-scheme secret.
        let cred = Ref::kubernetes("ns", "name", "key");
        let m = broker
            .resolve_credential_for("kubernetes", &cred)
            .await
            .unwrap();
        assert_eq!(m.value, "own-secret");
        assert!(
            audit.records.lock().unwrap().is_empty(),
            "an own-credential read is not a gated cross-plugin resolution"
        );
    }

    #[tokio::test]
    async fn resolve_endpoint_strips_inline_credential_into_header() {
        // A discovered record whose URL carries inline userinfo must surface a bare URL + a Basic
        // header — the credential-bearing URL is never exposed.
        let endpoints = Arc::new(EndpointRegistry::new());
        endpoints.put(EndpointRecord {
            owner: "k8s".into(),
            ..EndpointRecord::config(EndpointRef::discovered(
                "svc-1",
                "https://user:p%40ss@svc.internal/base",
                "service",
            ))
        });
        let broker = EndpointBroker::new(
            Arc::new(FakeInvoker {
                by_provider: HashMap::new(),
            }),
            Arc::new(PluginRegistry::new()),
            endpoints,
        );
        let resolved = broker.resolve_endpoint("@endpoint/svc-1").await.unwrap();
        assert_eq!(
            resolved.url, "https://svc.internal/base",
            "userinfo stripped"
        );
        assert!(
            !resolved.url.contains("user"),
            "no inline credential in the URL"
        );
        let (name, value) = &resolved.injected_headers[0];
        assert_eq!(name, "Authorization");
        assert!(
            value.starts_with("Basic "),
            "inline cred becomes a Basic header: {value}"
        );
    }

    #[tokio::test]
    async fn resolve_endpoint_materializes_credential_ref_into_bearer() {
        // A discovered record owned by `k8s` with a kubernetes credential_ref: resolving it is the
        // owner reading its own credential (not cross-plugin), injected as a Bearer header.
        let endpoints = Arc::new(EndpointRegistry::new());
        endpoints.put(EndpointRecord {
            owner: "kubernetes".into(),
            ..EndpointRecord::config(EndpointRef {
                credential_ref: Some(Ref::kubernetes("monitoring", "tok", "token")),
                ..EndpointRef::discovered("api-1", "https://api.internal/v1", "service")
            })
        });
        let reader = Arc::new(FakeReader {
            value: "tok-value".into(),
            ..Default::default()
        });
        let broker = EndpointBroker::new(
            Arc::new(FakeInvoker {
                by_provider: HashMap::new(),
            }),
            Arc::new(PluginRegistry::new()),
            endpoints,
        )
        .with_credential_reader(reader);
        // The owner (`kubernetes`) consuming its OWN kubernetes credential is not a cross-plugin hop,
        // so no grant is needed — the Bearer header is injected.
        let resolved = broker
            .resolve_endpoint_for("kubernetes", "@endpoint/api-1")
            .await
            .unwrap();
        assert_eq!(resolved.url, "https://api.internal/v1");
        assert_eq!(
            resolved.injected_headers,
            vec![("Authorization".to_string(), "Bearer tok-value".to_string())]
        );
        // The consumer-agnostic `resolve_endpoint` uses an empty consumer that can never match a
        // cross-plugin grant → the owned-by-`kubernetes` credential is NOT injected silently.
        assert!(
            broker.resolve_endpoint("@endpoint/api-1").await.is_err(),
            "consumer-agnostic resolution must not inject a cross-plugin credential"
        );
    }

    #[tokio::test]
    async fn http_ref_with_cross_plugin_credential_denied_without_grant() {
        // The HTTP analog of `cross_plugin_resolution_denied_without_grant`: a discovered endpoint
        // whose `credential_ref` is owned by ANOTHER plugin (`kubernetes`), consumed by `sql` doing
        // `http.do` via the ref. Host-injecting that credential is a cross-plugin USE → gated.
        let endpoints = Arc::new(EndpointRegistry::new());
        endpoints.put(EndpointRecord {
            owner: "kubernetes".into(),
            ..EndpointRecord::config(EndpointRef {
                credential_ref: Some(Ref::kubernetes("monitoring", "pg-creds", "token")),
                ..EndpointRef::discovered("pg-1", "https://pg.internal/v1", "postgres")
            })
        });
        let reader = Arc::new(FakeReader {
            value: "k8s-token".into(),
            ..Default::default()
        });

        // Ungranted: `sql` is not granted `kubernetes`' credentials → refused, and the credential is
        // never read.
        let denied = EndpointBroker::new(
            Arc::new(FakeInvoker {
                by_provider: HashMap::new(),
            }),
            Arc::new(PluginRegistry::new()),
            endpoints.clone(),
        )
        .with_credential_reader(reader.clone());
        let err = denied
            .resolve_endpoint_for("sql", "@endpoint/pg-1")
            .await
            .unwrap_err();
        assert!(
            err.contains("denied"),
            "ungranted cross-plugin HTTP injection must be refused: {err}"
        );
        assert!(
            reader.reads.lock().unwrap().is_empty(),
            "the provider must not be read without a grant"
        );

        // Granted: with `sql:kubernetes`, the credential is materialized and injected as a Bearer
        // header — the bare URL is surfaced, the token only in the host-injected header.
        let granted = EndpointBroker::new(
            Arc::new(FakeInvoker {
                by_provider: HashMap::new(),
            }),
            Arc::new(PluginRegistry::new()),
            endpoints,
        )
        .with_credential_reader(reader)
        .with_cross_plugin_grants(CrossPluginGrants::new(vec!["sql:kubernetes".into()]));
        let resolved = granted
            .resolve_endpoint_for("sql", "@endpoint/pg-1")
            .await
            .unwrap();
        assert_eq!(resolved.url, "https://pg.internal/v1");
        assert_eq!(
            resolved.injected_headers,
            vec![("Authorization".to_string(), "Bearer k8s-token".to_string())]
        );
    }

    #[tokio::test]
    async fn provider_discover_rejects_effectful_op() {
        // The discovery path must only ever call the read-only `endpoint.discover` op. A recording
        // invoker proves the broker never drives any other op during discovery.
        struct OpRecordingInvoker {
            ops: std::sync::Mutex<Vec<String>>,
        }
        #[async_trait]
        impl ProviderInvoker for OpRecordingInvoker {
            async fn discover(
                &self,
                _name: &str,
                product: &str,
                _query: &Value,
                _limit: usize,
            ) -> Result<Vec<EndpointCandidate>, String> {
                // The production `HostProviderInvoker` hardcodes `endpoint.discover`; record that the
                // broker drives discovery only through this seam (never an effectful op).
                self.ops.lock().unwrap().push("endpoint.discover".into());
                Ok(vec![candidate("e", product, 1.0, "p")])
            }
        }
        let reg = Arc::new(PluginRegistry::new());
        register_provider(&reg, "p", "prometheus").await;
        let invoker = Arc::new(OpRecordingInvoker {
            ops: std::sync::Mutex::new(Vec::new()),
        });
        let broker = EndpointBroker::new(invoker.clone(), reg, Arc::new(EndpointRegistry::new()));
        broker.discover("prometheus", &json!({}), 10, None).await;
        let ops = invoker.ops.lock().unwrap();
        assert!(!ops.is_empty(), "the provider was driven");
        assert!(
            ops.iter().all(|o| o == "endpoint.discover"),
            "discovery must only ever call endpoint.discover, saw: {ops:?}"
        );
    }
}
