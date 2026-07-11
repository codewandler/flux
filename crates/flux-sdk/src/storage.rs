//! Where a client's sessions live — the storage seam.
//!
//! [`Storage`] is the one injection point that decides whether an embedded agent is ephemeral or
//! durable. The default is in-memory (exactly the pre-0.16 behavior); [`Storage::dir`] makes
//! sessions survive the process — and with them turn history, suspended `await` flows, and the
//! projections a [`Session`](crate::Session) exposes.

use std::path::PathBuf;
use std::sync::Arc;

use flux_core::Result;
use flux_events::EventStore;
use flux_flow::state::FlowStore;

/// Where a [`Client`](crate::Client)'s sessions live.
///
/// Passed to [`ClientBuilder::storage`](crate::ClientBuilder::storage) (and
/// [`FlowClientBuilder::storage`](crate::FlowClientBuilder::storage)). The default is
/// [`Storage::in_memory`].
pub struct Storage(StorageInner);

enum StorageInner {
    InMemory,
    Dir(PathBuf),
    Custom {
        events: Arc<EventStore>,
        flow: FlowStore,
    },
}

impl Storage {
    /// Ephemeral in-memory stores (the default): sessions die with the process.
    pub fn in_memory() -> Self {
        Storage(StorageInner::InMemory)
    }

    /// Persistent stores under `dir`: `<dir>/events.db` + `<dir>/flow.db` — the exact layout the
    /// CLI uses, so a directory persisted by the SDK is readable by `flux sessions`,
    /// `flux replay`, and `flux fork`. The directory is created if missing.
    pub fn dir(path: impl Into<PathBuf>) -> Self {
        Storage(StorageInner::Dir(path.into()))
    }

    /// Bring your own stores — the escape hatch for anything the two conveniences don't cover
    /// (e.g. a Postgres-backed `EventStore::open_postgres`). The flow store should be built over
    /// the same event store (`FlowStore::open`/`in_memory_with_events`) so flow state and the
    /// event log stay one world.
    pub fn custom(events: Arc<EventStore>, flow: FlowStore) -> Self {
        Storage(StorageInner::Custom { events, flow })
    }

    /// Open/instantiate the stores this configuration describes — used by the agentic `Client`,
    /// which needs both the event store (sessions, history, projections) and the flow store.
    pub(crate) fn resolve(self) -> Result<(Arc<EventStore>, FlowStore)> {
        match self.0 {
            StorageInner::InMemory => {
                let events = Arc::new(EventStore::in_memory()?);
                let flow = FlowStore::in_memory_with_events(events.clone())?;
                Ok((events, flow))
            }
            StorageInner::Dir(dir) => {
                let events = Arc::new(EventStore::open(
                    Self::prepare_dir(&dir)?.join("events.db"),
                )?);
                let flow = FlowStore::open(dir.join("flow.db"), events.clone())?;
                Ok((events, flow))
            }
            StorageInner::Custom { events, flow } => Ok((events, flow)),
        }
    }

    /// Just the flow store — used by [`FlowClient`](crate::FlowClient), which has no event-store
    /// surface. For [`Storage::dir`] this persists `flow.db` only; unlike [`resolve`](Self::resolve)
    /// it does **not** create an `events.db` the flow lifecycle would never read (the flow store's
    /// required event sink is a throwaway in-memory one).
    pub(crate) fn into_flow_store(self) -> Result<FlowStore> {
        match self.0 {
            StorageInner::InMemory => FlowStore::in_memory(),
            StorageInner::Dir(dir) => {
                let events = Arc::new(EventStore::in_memory()?);
                FlowStore::open(Self::prepare_dir(&dir)?.join("flow.db"), events)
            }
            StorageInner::Custom { flow, .. } => Ok(flow),
        }
    }

    fn prepare_dir(dir: &std::path::Path) -> Result<&std::path::Path> {
        std::fs::create_dir_all(dir).map_err(|e| {
            flux_core::Error::Other(format!("create storage dir {}: {e}", dir.display()))
        })?;
        Ok(dir)
    }
}

impl Default for Storage {
    fn default() -> Self {
        Storage::in_memory()
    }
}

impl std::fmt::Debug for Storage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            StorageInner::InMemory => f.write_str("Storage::in_memory"),
            StorageInner::Dir(d) => write!(f, "Storage::dir({})", d.display()),
            StorageInner::Custom { .. } => f.write_str("Storage::custom"),
        }
    }
}
