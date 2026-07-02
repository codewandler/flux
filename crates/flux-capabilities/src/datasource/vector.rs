//! [`VectorStore`] — the persistence seam for embedding vectors (story D-51).
//!
//! [`SemanticIndex`](super::SemanticIndex) stores one vector per record, addressed by the record's
//! `(source, entity, id)`. Where those vectors live is this trait's concern: the default
//! [`MemoryVectorStore`] holds them in a map (rebuilt on ingest), while a durable backing — e.g. a
//! `sqlite-vec` table co-located in the `SqliteBackend` DB — implements the same trait so vectors survive a
//! restart with no re-embed. Keeping the seam generic means the store is swappable without touching the
//! rerank logic.

use std::collections::HashMap;
use std::sync::Mutex;

use flux_core::Result;

/// A record's primary key: `(source_key, entity, id)`.
pub type VectorAddr = (String, String, String);

/// Where [`SemanticIndex`](super::SemanticIndex) persists embedding vectors. Addressed by a record's
/// `(source, entity, id)`; all methods take `&self` (interior mutability) so one store is shared as
/// `Arc<dyn VectorStore>`.
pub trait VectorStore: Send + Sync {
    /// Insert or replace the vector for `addr`.
    fn upsert(&self, addr: VectorAddr, vector: Vec<f32>) -> Result<()>;
    /// Fetch the vector for `addr`, if present.
    fn get(&self, addr: &VectorAddr) -> Result<Option<Vec<f32>>>;
    /// Drop every vector under one source key; returns how many were removed.
    fn delete_source(&self, source: &str) -> Result<usize>;
    /// Drop the vectors of specific ids of one entity in one source; returns how many were removed.
    fn delete(&self, source: &str, entity: &str, ids: &[String]) -> Result<usize>;
    /// Drop every vector.
    fn clear(&self) -> Result<()>;
}

/// The default in-memory [`VectorStore`] — a `HashMap` rebuilt on ingest. Not durable; a `sqlite-vec`
/// backing replaces it where vectors must survive a restart.
#[derive(Default)]
pub struct MemoryVectorStore {
    vectors: Mutex<HashMap<VectorAddr, Vec<f32>>>,
}

impl MemoryVectorStore {
    /// An empty in-memory vector store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl VectorStore for MemoryVectorStore {
    fn upsert(&self, addr: VectorAddr, vector: Vec<f32>) -> Result<()> {
        self.vectors
            .lock()
            .expect("vector store poisoned")
            .insert(addr, vector);
        Ok(())
    }

    fn get(&self, addr: &VectorAddr) -> Result<Option<Vec<f32>>> {
        Ok(self
            .vectors
            .lock()
            .expect("vector store poisoned")
            .get(addr)
            .cloned())
    }

    fn delete_source(&self, source: &str) -> Result<usize> {
        let mut m = self.vectors.lock().expect("vector store poisoned");
        let before = m.len();
        m.retain(|(s, _, _), _| s != source);
        Ok(before - m.len())
    }

    fn delete(&self, source: &str, entity: &str, ids: &[String]) -> Result<usize> {
        let mut m = self.vectors.lock().expect("vector store poisoned");
        let before = m.len();
        m.retain(|(s, e, i), _| !(s == source && e == entity && ids.iter().any(|x| x == i)));
        Ok(before - m.len())
    }

    fn clear(&self) -> Result<()> {
        self.vectors.lock().expect("vector store poisoned").clear();
        Ok(())
    }
}

#[cfg(feature = "sqlite-vec")]
pub use sqlite_vec_store::SqliteVecStore;

/// A durable [`VectorStore`] backed by the `sqlite-vec` extension (story D-51), behind the `sqlite-vec`
/// feature. Vectors live in a `vec0` virtual table in the same SQLite file as the records, so they survive
/// a restart with no re-embed; a plain companion table maps each vector's composite address to its source
/// for lifecycle deletes. Feature-gated + not built in the default gate (needs the vendored extension +
/// rusqlite FFI), so this is unverified in the hermetic gate — see the story's residual notes.
#[cfg(feature = "sqlite-vec")]
mod sqlite_vec_store {
    use std::path::Path;
    use std::sync::{Mutex, Once};

    use rusqlite::{params, Connection, OptionalExtension};

    use flux_core::{Error, Result};

    use super::{VectorAddr, VectorStore};

    fn map<E: std::fmt::Display>(e: E) -> Error {
        Error::Other(format!("sqlite-vec store: {e}"))
    }

    /// The composite key for a vector row: `source␟entity␟id` (unit-separator delimited).
    fn addr_key(source: &str, entity: &str, id: &str) -> String {
        format!("{source}\u{1f}{entity}\u{1f}{id}")
    }

    /// Register the sqlite-vec extension entry point for every subsequently-opened connection (once).
    fn register_extension() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| unsafe {
            rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute(
                sqlite_vec::sqlite3_vec_init as *const (),
            )));
        });
    }

    /// A `sqlite-vec`-backed durable [`VectorStore`].
    pub struct SqliteVecStore {
        conn: Mutex<Connection>,
    }

    impl SqliteVecStore {
        /// Open (creating if needed) a store at `path` holding `dim`-dimensional vectors.
        pub fn open(path: impl AsRef<Path>, dim: usize) -> Result<Self> {
            register_extension();
            Self::init(Connection::open(path).map_err(map)?, dim)
        }

        /// An in-memory store (tests / ephemeral).
        pub fn in_memory(dim: usize) -> Result<Self> {
            register_extension();
            Self::init(Connection::open_in_memory().map_err(map)?, dim)
        }

        fn init(conn: Connection, dim: usize) -> Result<Self> {
            conn.execute_batch(&format!(
                "CREATE VIRTUAL TABLE IF NOT EXISTS vec_store USING vec0(
                     addr TEXT PRIMARY KEY,
                     embedding float[{dim}]
                 );
                 CREATE TABLE IF NOT EXISTS vec_meta (
                     addr TEXT PRIMARY KEY,
                     source TEXT NOT NULL,
                     entity TEXT NOT NULL,
                     id TEXT NOT NULL
                 );"
            ))
            .map_err(map)?;
            Ok(Self {
                conn: Mutex::new(conn),
            })
        }
    }

    impl VectorStore for SqliteVecStore {
        fn upsert(&self, addr: VectorAddr, vector: Vec<f32>) -> Result<()> {
            let key = addr_key(&addr.0, &addr.1, &addr.2);
            let json = serde_json::to_string(&vector).map_err(map)?;
            let conn = self.conn.lock().expect("sqlite-vec poisoned");
            conn.execute("DELETE FROM vec_store WHERE addr = ?1", params![key])
                .map_err(map)?;
            conn.execute(
                "INSERT INTO vec_store(addr, embedding) VALUES (?1, ?2)",
                params![key, json],
            )
            .map_err(map)?;
            conn.execute(
                "INSERT OR REPLACE INTO vec_meta(addr, source, entity, id) VALUES (?1, ?2, ?3, ?4)",
                params![key, addr.0, addr.1, addr.2],
            )
            .map_err(map)?;
            Ok(())
        }

        fn get(&self, addr: &VectorAddr) -> Result<Option<Vec<f32>>> {
            let key = addr_key(&addr.0, &addr.1, &addr.2);
            let conn = self.conn.lock().expect("sqlite-vec poisoned");
            let blob: Option<Vec<u8>> = conn
                .query_row(
                    "SELECT embedding FROM vec_store WHERE addr = ?1",
                    params![key],
                    |r| r.get(0),
                )
                .optional()
                .map_err(map)?;
            Ok(blob.map(|b| {
                b.chunks_exact(4)
                    .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                    .collect()
            }))
        }

        fn delete_source(&self, source: &str) -> Result<usize> {
            let conn = self.conn.lock().expect("sqlite-vec poisoned");
            let keys: Vec<String> = conn
                .prepare("SELECT addr FROM vec_meta WHERE source = ?1")
                .map_err(map)?
                .query_map(params![source], |r| r.get(0))
                .map_err(map)?
                .collect::<std::result::Result<_, _>>()
                .map_err(map)?;
            for key in &keys {
                conn.execute("DELETE FROM vec_store WHERE addr = ?1", params![key])
                    .map_err(map)?;
            }
            conn.execute("DELETE FROM vec_meta WHERE source = ?1", params![source])
                .map_err(map)?;
            Ok(keys.len())
        }

        fn delete(&self, source: &str, entity: &str, ids: &[String]) -> Result<usize> {
            let conn = self.conn.lock().expect("sqlite-vec poisoned");
            let mut n = 0;
            for id in ids {
                let key = addr_key(source, entity, id);
                n += conn
                    .execute("DELETE FROM vec_store WHERE addr = ?1", params![key])
                    .map_err(map)?;
                conn.execute("DELETE FROM vec_meta WHERE addr = ?1", params![key])
                    .map_err(map)?;
            }
            Ok(n)
        }

        fn clear(&self) -> Result<()> {
            let conn = self.conn.lock().expect("sqlite-vec poisoned");
            conn.execute("DELETE FROM vec_store", []).map_err(map)?;
            conn.execute("DELETE FROM vec_meta", []).map_err(map)?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_upsert_get_and_delete() {
        let s = MemoryVectorStore::new();
        let a: VectorAddr = ("local".into(), "file.document".into(), "x".into());
        s.upsert(a.clone(), vec![1.0, 0.0]).unwrap();
        assert_eq!(s.get(&a).unwrap(), Some(vec![1.0, 0.0]));
        assert_eq!(
            s.delete("local", "file.document", &["x".into()]).unwrap(),
            1
        );
        assert_eq!(s.get(&a).unwrap(), None);
    }

    #[test]
    fn delete_source_scopes_to_one_source() {
        let s = MemoryVectorStore::new();
        s.upsert(("a".into(), "e".into(), "1".into()), vec![1.0])
            .unwrap();
        s.upsert(("b".into(), "e".into(), "1".into()), vec![1.0])
            .unwrap();
        assert_eq!(s.delete_source("a").unwrap(), 1);
        assert_eq!(s.get(&("a".into(), "e".into(), "1".into())).unwrap(), None);
        assert!(s
            .get(&("b".into(), "e".into(), "1".into()))
            .unwrap()
            .is_some());
    }
}
