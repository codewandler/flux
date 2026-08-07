//! Host registry — named execution-substrate bindings (Decision 0018 / C-648).
//!
//! The session-scoped [`HostRegistry`] holds config-declared [`HostRecord`]s (keyed by binding
//! name) and follows the [`EndpointRegistry`](crate::EndpointRegistry) persistence pattern: an
//! optional TOML store holding weak references only — the persisted form carries just the
//! `credential_ref` location, re-resolved live each session, never a secret. Resolution of a
//! binding to a live `ExecutionSystem` lives in the surface crate (C-650), not here.

mod ops;

pub use ops::{host_tools, register_host_ops, try_register_host_ops, HOST_GROUP};

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::RwLock;

use async_trait::async_trait;
use serde::Serialize;

use flux_core::{Error, Result};
use flux_secret::host::{HostBackend, HostRecord, HostRef};

/// A session-scoped registry of host bindings, keyed by binding name. Config-declared hosts are
/// registered at session start; ephemeral (session-only) bindings may join in memory and are never
/// persisted by any production path.
pub struct HostRegistry {
    records: RwLock<HashMap<String, HostRecord>>,
    /// Where records persist; `None` disables persistence (tests / ephemeral use).
    path: Option<PathBuf>,
}

impl Default for HostRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl HostRegistry {
    /// An in-memory-only registry (no persistence).
    pub fn new() -> Self {
        Self {
            records: RwLock::new(HashMap::new()),
            path: None,
        }
    }

    /// A registry backed by `path`.
    pub fn with_path(path: PathBuf) -> Self {
        Self {
            records: RwLock::new(HashMap::new()),
            path: Some(path),
        }
    }

    /// `~/.flux/hosts.toml`, if `$HOME` is set.
    pub fn default_path() -> Option<PathBuf> {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".flux").join("hosts.toml"))
    }

    /// Insert or replace a record by its binding name.
    pub fn put(&self, record: HostRecord) {
        self.records
            .write()
            .unwrap()
            .insert(record.host.id.clone(), record);
    }

    /// Answer a record by binding name (the weak ref — no secret).
    pub fn get(&self, id: &str) -> Option<HostRecord> {
        self.records.read().unwrap().get(id).cloned()
    }

    /// Remove a record by binding name, returning it if present (the store half of `flux host rm`).
    pub fn remove(&self, id: &str) -> Option<HostRecord> {
        self.records.write().unwrap().remove(id)
    }

    /// Whether the registry holds no bindings at all. Cheap — the ambient-signal wiring asks this
    /// once at startup on the loaded registry instead of re-reading the store.
    pub fn is_empty(&self) -> bool {
        self.records.read().unwrap().is_empty()
    }

    /// All records, sorted by binding name for stable display.
    pub fn list(&self) -> Vec<HostRecord> {
        let mut v: Vec<HostRecord> = self.records.read().unwrap().values().cloned().collect();
        v.sort_by(|a, b| a.host.id.cmp(&b.host.id));
        v
    }

    /// The known binding names, sorted — what an unknown-name refusal lists (C-650).
    pub fn known_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.records.read().unwrap().keys().cloned().collect();
        v.sort();
        v
    }

    /// Load persisted records from `path` into memory (merge). A missing file is fine; a corrupt
    /// one is an error so a later `save` cannot silently clobber it.
    pub fn load(&self) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        // flux-allow-direct-io: host registry persistence — `path` is the host-configured store
        // (`self.path`), set at construction, not a model-directed path.
        let body = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(Error::Other(format!("read {}: {e}", path.display()))),
        };
        let persisted: Persisted = toml::from_str(&body).map_err(|e| {
            Error::Other(format!(
                "hosts store {} is corrupt ({e}); fix or remove it",
                path.display()
            ))
        })?;
        let mut guard = self.records.write().unwrap();
        for r in persisted.host {
            guard.insert(r.host.id.clone(), r);
        }
        Ok(())
    }

    /// Persist all current records to `path` atomically (temp file + rename). The file is **not**
    /// secret (weak refs only), so it is written 0644.
    pub fn save(&self) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            // flux-allow-direct-io: host registry persistence — create the host-configured store
            // dir (`self.path`'s parent), not a model-directed path.
            std::fs::create_dir_all(dir)
                .map_err(|e| Error::Other(format!("create {}: {e}", dir.display())))?;
        }
        let persisted = Persisted { host: self.list() };
        let body = toml::to_string_pretty(&persisted)
            .map_err(|e| Error::Other(format!("serialize hosts: {e}")))?;
        let tmp = path.with_extension("toml.tmp");
        // flux-allow-direct-io: host registry persistence — atomic write to a temp beside the
        // host-configured store (`self.path`), not a model-directed path.
        std::fs::write(&tmp, body.as_bytes())
            .map_err(|e| Error::Other(format!("write {}: {e}", tmp.display())))?;
        // flux-allow-direct-io: host registry persistence — atomic rename into the
        // host-configured store (`self.path`), not a model-directed path.
        std::fs::rename(&tmp, path)
            .map_err(|e| Error::Other(format!("rename into {}: {e}", path.display())))?;
        Ok(())
    }
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Persisted {
    #[serde(default)]
    host: Vec<HostRecord>,
}

/// The static (no-side-effect) availability statement for a backend — what `host ls`/`show` may
/// honestly claim without probing. `local` is the running substrate; a `remote` binding is only a
/// declaration until `probe` verifies it; the peer backends fail closed until their selection
/// stories wire them.
pub fn static_availability(backend: HostBackend) -> &'static str {
    match backend {
        HostBackend::Local => "available",
        HostBackend::Remote => "declared (verify with probe)",
        HostBackend::Sandboxed | HostBackend::Container | HostBackend::Kubernetes => {
            "unwired (selection fails closed)"
        }
    }
}

/// A backend's side-effect-free identity check result (C-649 `probe`): the resolved
/// `SubstrateIdentity` fields, plus the negotiated protocol version for a remote backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostProbeReport {
    pub kind: String,
    pub workspace: String,
    pub confinement: String,
    pub remotely_reported: bool,
    /// The negotiated remote-system protocol version; `None` for a local identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol_version: Option<u32>,
}

/// Why a probe could not produce an identity — typed, not stringly (C-649). The classes are the
/// contract; `detail` strings carry transport specifics for display only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum HostProbeFailure {
    /// The binding names a credential the current environment cannot supply.
    CredentialUnavailable { reference: String, detail: String },
    /// The backend kind has no probe-able implementation wired yet; selection fails closed too.
    BackendUnwired { backend: String },
    /// The transport-level identity check failed (unreachable, refused, handshake error).
    Connect { detail: String },
}

impl fmt::Display for HostProbeFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CredentialUnavailable { reference, detail } => {
                write!(f, "credential `{reference}` unavailable: {detail}")
            }
            Self::BackendUnwired { backend } => {
                write!(
                    f,
                    "backend `{backend}` has no probe-able implementation wired yet"
                )
            }
            Self::Connect { detail } => write!(f, "identity check failed: {detail}"),
        }
    }
}

impl std::error::Error for HostProbeFailure {}

/// Performs a backend's side-effect-free identity check (C-649 `probe`). Implemented in the
/// surface crate — the remote handshake client lives above this layer — and shared by the
/// `host.probe` op and the `flux host probe` command so the two cannot drift.
#[async_trait]
pub trait HostProber: Send + Sync {
    async fn probe(&self, host: &HostRef)
        -> std::result::Result<HostProbeReport, HostProbeFailure>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_secret::host::{HostBackend, HostRef};
    use flux_secret::Ref;

    fn temp_store(tag: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!("flux-host-{tag}-{}", std::process::id()))
            .join("hosts.toml")
    }

    fn farm() -> HostRecord {
        HostRecord::config(HostRef {
            url: Some("https://farm.example:8443".into()),
            credential_ref: Some(Ref::env("FARM_TOKEN")),
            ..HostRef::declared("build-farm", HostBackend::Remote)
        })
    }

    #[test]
    fn registry_answers_list_and_get_by_id() {
        let reg = HostRegistry::new();
        assert!(reg.is_empty());
        reg.put(farm());
        reg.put(HostRecord::config(HostRef::declared(
            "here",
            HostBackend::Local,
        )));
        assert!(!reg.is_empty());
        let got = reg.get("build-farm").expect("registered binding resolves");
        assert_eq!(got.host.backend, HostBackend::Remote);
        assert!(reg.get("nowhere").is_none());
        // Sorted by name for stable display; names are what a refusal lists.
        let names: Vec<String> = reg.list().iter().map(|r| r.host.id.clone()).collect();
        assert_eq!(names, ["build-farm", "here"]);
        assert_eq!(reg.known_names(), ["build-farm", "here"]);
    }

    #[test]
    fn registry_save_load_round_trips_weak_refs_only() {
        let path = temp_store("roundtrip");
        let reg = HostRegistry::with_path(path.clone());
        reg.put(farm());
        reg.save().unwrap();
        // The persisted form carries the credential *location*, never a value — there is no
        // value anywhere in the pipeline to leak.
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(
            body.contains("scheme = \"env\"") && body.contains("FARM_TOKEN"),
            "location persisted: {body}"
        );
        assert!(body.contains("backend = \"remote\""), "{body}");

        let back = HostRegistry::with_path(path.clone());
        back.load().unwrap();
        assert_eq!(back.get("build-farm"), Some(farm()));
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn a_corrupt_store_is_an_error_not_a_silent_clobber() {
        let path = temp_store("corrupt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "host = 3\n").unwrap();
        let reg = HostRegistry::with_path(path.clone());
        let err = reg.load().unwrap_err();
        assert!(err.to_string().contains("corrupt"), "{err}");
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn entries_expose_backend_and_address_without_credential_material() {
        // C-648: display data (backend kind + address) is available from the record itself; the
        // only credential field is a `Ref` location, and no registry path resolves it.
        let rec = farm();
        assert_eq!(rec.host.backend.as_str(), "remote");
        assert_eq!(rec.host.display_address(), "https://farm.example:8443");
        assert_eq!(
            rec.host.credential_ref.as_ref().map(|r| r.to_string()),
            Some("env/FARM_TOKEN".to_string())
        );
    }
}
