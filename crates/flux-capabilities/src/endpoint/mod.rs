//! Endpoint registry + reference resolution (L5).
//!
//! The session-scoped [`EndpointRegistry`] holds discovered + config-bound [`EndpointRecord`]s
//! (keyed by id, with owner/TTL), and persists imported ones to `~/.flux/endpoints.toml` — weak
//! references only, never a secret (the persisted form carries just the `credential_ref` location,
//! re-resolved live each session). [`StaticResolver`] implements [`ReferenceResolver`] for the
//! config/manifest-default source: a named reference resolves to its bound URL, and an `env`-scheme
//! credential materializes host-side. Discovery + cross-plugin credential resolution layer on top in
//! the broker (D-26/D-27).

mod broker;
mod host_caps;
mod ops;

pub use broker::{
    CredentialReader, CrossPluginApprover, CrossPluginAudit, CrossPluginGrants, EndpointBroker,
    HostCredentialReader, HostProviderInvoker, PluginRegistry, ProviderEntry, ProviderInvoker,
};
pub use host_caps::EndpointBrokerHostCaps;
pub use ops::{endpoint_tools, register_endpoint_ops, ENDPOINT_GROUP};

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;

use flux_plugin::ReferenceResolver;
use flux_secret::endpoint::{EndpointRecord, EndpointRef, ResolvedEndpoint, SourceKind};
use flux_secret::{Kind, Material, Ref, Scheme};
use flux_system::System;

/// A session-scoped registry of endpoint records, keyed by reference id. Discovered endpoints live
/// in memory; imported ones are also persisted (weak-ref only) to `~/.flux/endpoints.toml`.
pub struct EndpointRegistry {
    records: RwLock<HashMap<String, EndpointRecord>>,
    /// Where imported records persist; `None` disables persistence (tests / ephemeral use).
    path: Option<PathBuf>,
}

impl Default for EndpointRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl EndpointRegistry {
    /// An in-memory-only registry (no persistence).
    pub fn new() -> Self {
        Self {
            records: RwLock::new(HashMap::new()),
            path: None,
        }
    }

    /// A registry backed by `path` for imported records.
    pub fn with_path(path: PathBuf) -> Self {
        Self {
            records: RwLock::new(HashMap::new()),
            path: Some(path),
        }
    }

    /// `~/.flux/endpoints.toml`, if `$HOME` is set.
    pub fn default_path() -> Option<PathBuf> {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".flux").join("endpoints.toml"))
    }

    /// Insert or replace a record by its endpoint id.
    pub fn put(&self, record: EndpointRecord) {
        self.records
            .write()
            .unwrap()
            .insert(record.endpoint.id.clone(), record);
    }

    /// Resolve a record by reference id (the weak ref — no secret).
    pub fn resolve(&self, id: &str) -> Option<EndpointRecord> {
        self.records.read().unwrap().get(id).cloned()
    }

    /// All records, sorted by id for stable display.
    pub fn list(&self) -> Vec<EndpointRecord> {
        let mut v: Vec<EndpointRecord> = self.records.read().unwrap().values().cloned().collect();
        v.sort_by(|a, b| a.endpoint.id.cmp(&b.endpoint.id));
        v
    }

    /// Replace exactly the set owned by `owner`, leaving other owners' records untouched. This is
    /// how a provider refreshes its discoveries without disturbing the rest (fluxplane's
    /// `ReplaceOwned`).
    pub fn replace_owned(&self, owner: &str, records: Vec<EndpointRecord>) {
        let mut guard = self.records.write().unwrap();
        guard.retain(|_, r| r.owner != owner);
        for r in records {
            guard.insert(r.endpoint.id.clone(), r);
        }
    }

    /// Load persisted records from `path` into memory (merge). A missing file is fine; a corrupt one
    /// is an error so a later `save` cannot silently clobber it.
    pub fn load(&self) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let body = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(format!("read {}: {e}", path.display())),
        };
        let persisted: Persisted = toml::from_str(&body).map_err(|e| {
            format!(
                "endpoints store {} is corrupt ({e}); fix or remove it",
                path.display()
            )
        })?;
        let mut guard = self.records.write().unwrap();
        for r in persisted.endpoint {
            guard.insert(r.endpoint.id.clone(), r);
        }
        Ok(())
    }

    /// Persist all current records to `path` atomically (temp file + rename). The file is **not**
    /// secret (weak refs only), so it is written 0644.
    pub fn save(&self) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        }
        let persisted = Persisted {
            endpoint: self.list(),
        };
        let body =
            toml::to_string_pretty(&persisted).map_err(|e| format!("serialize endpoints: {e}"))?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, body.as_bytes())
            .map_err(|e| format!("write {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("rename into {}: {e}", path.display()))?;
        Ok(())
    }
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Persisted {
    #[serde(default)]
    endpoint: Vec<EndpointRecord>,
}

/// Resolves **named** (config/manifest-default) endpoint references from a host-side binding map,
/// and materializes `env`-scheme credential references through the guarded [`System`]. This is the
/// static first link in the resolver chain; discovery + cross-plugin (`kubernetes`-scheme)
/// resolution layer on top in the broker (D-26/D-27).
pub struct StaticResolver {
    system: Arc<System>,
    bindings: HashMap<String, EndpointRef>,
}

impl StaticResolver {
    /// `bindings` maps a named reference (`"sql.endpoint"`) to its config-bound [`EndpointRef`].
    pub fn new(system: Arc<System>, bindings: HashMap<String, EndpointRef>) -> Self {
        Self { system, bindings }
    }

    fn materialize(&self, reference: &Ref) -> Result<Material, String> {
        match reference.scheme {
            Scheme::Env => {
                let value = self
                    .system
                    .env(&reference.slot)
                    .ok_or_else(|| format!("no env value for credential `{reference}`"))?;
                Ok(Material {
                    reference: reference.clone(),
                    kind: Kind::ApiKey,
                    value,
                    media_type: None,
                })
            }
            // plugin/kubernetes-scheme refs are resolved by the broker (cross-plugin) — not here.
            _ => Err(format!(
                "static resolver cannot materialize `{reference}` (needs the discovery broker)"
            )),
        }
    }
}

#[async_trait]
impl ReferenceResolver for StaticResolver {
    async fn resolve_endpoint(&self, reference: &str) -> Result<ResolvedEndpoint, String> {
        let ep = self
            .bindings
            .get(reference)
            .filter(|e| e.source == SourceKind::Config)
            .ok_or_else(|| format!("no config binding for endpoint `{reference}`"))?;
        let mut resolved = ResolvedEndpoint::new(reference, &ep.url);
        if let Some(cred) = &ep.credential_ref {
            let material = self.materialize(cred)?;
            // Default HTTP injection for a config-bound endpoint is a bearer header; richer schemes
            // are handled by the broker/host-caps path in D-27.
            resolved = resolved.with_header("Authorization", format!("Bearer {}", material.value));
        }
        Ok(resolved)
    }

    async fn resolve_credential(&self, reference: &Ref) -> Result<Material, String> {
        self.materialize(reference)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_system::Workspace;

    fn test_system() -> Arc<System> {
        let dir = std::env::temp_dir().join(format!("flux-endpoint-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Arc::new(System::new(Workspace::new(&dir).unwrap()))
    }

    #[test]
    fn registry_put_resolve_replace_owned() {
        let reg = EndpointRegistry::new();
        let k8s_a = EndpointRecord {
            owner: "kubernetes".into(),
            ..EndpointRecord::config(EndpointRef::discovered(
                "pg-a",
                "postgres://a:5432/x",
                "postgres",
            ))
        };
        let k8s_b = EndpointRecord {
            owner: "kubernetes".into(),
            ..EndpointRecord::config(EndpointRef::discovered(
                "pg-b",
                "postgres://b:5432/x",
                "postgres",
            ))
        };
        let other = EndpointRecord {
            owner: "config".into(),
            ..EndpointRecord::config(EndpointRef::named("sql.endpoint", "postgres://c:5432/x"))
        };
        reg.put(k8s_a.clone());
        reg.put(k8s_b);
        reg.put(other.clone());
        assert_eq!(reg.resolve(&k8s_a.endpoint.id).unwrap(), k8s_a);
        assert_eq!(reg.list().len(), 3);

        // A provider refresh replaces only its own records.
        let k8s_a2 = EndpointRecord {
            owner: "kubernetes".into(),
            ..EndpointRecord::config(EndpointRef::discovered(
                "pg-a",
                "postgres://a2:5432/x",
                "postgres",
            ))
        };
        reg.replace_owned("kubernetes", vec![k8s_a2.clone()]);
        assert_eq!(reg.list().len(), 2); // pg-b dropped, pg-a replaced, config kept
        assert_eq!(
            reg.resolve(&k8s_a2.endpoint.id).unwrap().endpoint.url,
            "postgres://a2:5432/x"
        );
        assert!(reg.resolve(&other.endpoint.id).is_some());
    }

    #[test]
    fn registry_save_load_round_trips_weak_refs_only() {
        let dir = std::env::temp_dir().join(format!("flux-endpoint-store-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("endpoints.toml");

        let reg = EndpointRegistry::with_path(path.clone());
        let rec = EndpointRecord {
            owner: "kubernetes".into(),
            ttl_secs: Some(900),
            ..EndpointRecord::config(EndpointRef {
                credential_ref: Some(Ref::kubernetes("monitoring", "pg-creds", "password")),
                ..EndpointRef::discovered("pg-1", "postgres://db:5432/app", "postgres")
            })
        };
        reg.put(rec.clone());
        reg.save().unwrap();

        // The persisted file carries no secret value — only the credential *reference*.
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(!on_disk.contains("password=") && !on_disk.to_lowercase().contains("secret"));

        let reloaded = EndpointRegistry::with_path(path);
        reloaded.load().unwrap();
        assert_eq!(reloaded.resolve(&rec.endpoint.id).unwrap(), rec);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn static_resolver_binds_from_host_config() {
        let mut bindings = HashMap::new();
        bindings.insert(
            "sql.endpoint".to_string(),
            EndpointRef::named("sql.endpoint", "postgres://db.example:5432/app"),
        );
        let resolver = StaticResolver::new(test_system(), bindings);

        let resolved = resolver.resolve_endpoint("sql.endpoint").await.unwrap();
        assert_eq!(resolved.url, "postgres://db.example:5432/app");
        // An unknown reference is an error (not a silent default).
        assert!(resolver.resolve_endpoint("unknown.endpoint").await.is_err());
        // A kubernetes-scheme credential is the broker's job, not the static resolver's.
        assert!(resolver
            .resolve_credential(&Ref::kubernetes("ns", "name", "key"))
            .await
            .is_err());
    }
}
