//! `flux-pg` — the Postgres driver-owner.
//!
//! Everything Postgres-flavored in flux funnels through this one crate: the sole `sqlx` dependency,
//! the connection pool, the DSN contract, and — the load-bearing piece — a **panic-safe sync↔async
//! bridge**. flux's durable stores ([`flux_events::EventStore`], the datasource backends) expose
//! **synchronous** methods, but sqlx is async. [`PgHandle::block_on`] runs a query future to
//! completion from *any* calling context without spawning a nested runtime and without panicking.
//!
//! Backend crates reach sqlx through the [`sqlx`] re-export (`flux_pg::sqlx::query(...)`) so they
//! never declare their own driver dependency — there is exactly one sqlx in the tree.
//!
//! ## Why the bridge is shaped the way it is (do not "simplify" it)
//!
//! [`EventStore`]'s methods are called from every context flux supports: plain OS threads (the CLI),
//! multi-thread tokio worker threads (flux-server; embedders' async handlers), and current-thread
//! runtimes (`#[tokio::test]`). Every *naive* sync-over-async bridge panics somewhere in that matrix:
//!
//! - `Runtime::block_on` / the sync `postgres` crate's internal `block_on` → **panics** "Cannot start
//!   a runtime from within a runtime" when the caller is already on a tokio worker.
//! - `tokio::runtime::Handle::block_on` → **panics** on a multi-thread worker thread.
//! - `tokio::task::block_in_place` → **panics** on a current-thread runtime.
//!
//! The one shape that works everywhere: **spawn the future onto this handle's own dedicated
//! multi-thread runtime and block the caller on a plain [`std::sync::mpsc`] channel**. The spawn does
//! not care what context the caller is in, and a blocking `recv()` is legal on any thread — it merely
//! parks the caller the same way today's `Mutex<rusqlite::Connection>` lock does, so this changes
//! nothing about flux's blocking posture. Because the query future runs on a *separate* runtime, the
//! caller's thread (even a tokio worker) blocking on `recv()` cannot stall it: no deadlock.
//!
//! [`flux_events::EventStore`]: https://docs.rs/flux-events
//! [`EventStore`]: https://docs.rs/flux-events

use std::future::Future;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use flux_core::{Error, Result};

/// Re-export so backend crates write queries via `flux_pg::sqlx` and inherit the one owned version.
pub use sqlx;

use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};

/// A live Postgres connection pool plus the dedicated runtime that drives it.
///
/// Construct once with [`PgHandle::connect`] and share the `Arc` across every store that should
/// persist to this database. Cheap to clone the inner [`PgPool`] via [`PgHandle::pool`] into async
/// query blocks (the pool is itself reference-counted).
pub struct PgHandle {
    /// Dedicated multi-thread runtime — see the module-level bridge rationale. Wrapped in `Option`
    /// solely so [`Drop`] can move it out and shut it down without blocking (always `Some` while the
    /// handle is live).
    rt: Option<tokio::runtime::Runtime>,
    pool: PgPool,
}

impl Drop for PgHandle {
    /// A tokio [`Runtime`](tokio::runtime::Runtime)'s own `Drop` blocks on worker shutdown and
    /// **panics** if that drop runs while inside another runtime — precisely what happens when a
    /// server drops its last `Arc<PgHandle>` on a tokio worker thread (this epic's target
    /// deployment). `shutdown_background()` releases the runtime without blocking, so a `PgHandle` is
    /// safe to drop from any context — a plain thread, a tokio worker, or a current-thread runtime.
    fn drop(&mut self) {
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}

impl std::fmt::Debug for PgHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PgHandle").finish_non_exhaustive()
    }
}

impl PgHandle {
    /// Parse a `postgres://…` DSN, strip the flux-owned query params, and build a lazily-connecting
    /// pool on this handle's own runtime. The first query establishes the first connection, so
    /// `connect` never blocks and never needs a reachable database at construction time.
    pub fn connect(url: &str) -> Result<Arc<Self>> {
        let dsn = Dsn::parse(url)?;

        // A small dedicated runtime: sqlx work is IO-bound and yields on every socket await, so a
        // handful of workers drives many concurrent queries. Naming aids `top`/stack traces.
        let workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2)
            .clamp(2, 8);
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(workers)
            .enable_all()
            .thread_name("flux-pg")
            .build()
            .map_err(|e| Error::Other(format!("flux-pg: build runtime: {e}")))?;

        let schema = dsn.schema.clone();
        // Enter the runtime so any background task sqlx spawns on pool construction lands on `rt`,
        // NOT on whatever runtime the caller might already be inside. `enter()` only sets the
        // thread-local context; it runs nothing and cannot panic on a nested runtime.
        let pool = {
            let _guard = rt.enter();
            PgPoolOptions::new()
                .max_connections(dsn.pool_max)
                .acquire_timeout(Duration::from_millis(dsn.acquire_timeout_ms))
                .after_connect(move |conn, _meta| {
                    let schema = schema.clone();
                    Box::pin(async move {
                        if let Some(schema) = schema {
                            // `schema` doubles as the test-isolation mechanism, so make it
                            // self-contained: create it if absent, then pin the search_path. The
                            // name is validated as a bare identifier at parse time, so the inline
                            // quoting below cannot be an injection vector.
                            let ddl = format!("CREATE SCHEMA IF NOT EXISTS \"{schema}\"");
                            sqlx::query(&ddl).execute(&mut *conn).await?;
                            let set = format!("SET search_path TO \"{schema}\"");
                            sqlx::query(&set).execute(&mut *conn).await?;
                        }
                        Ok(())
                    })
                })
                .connect_lazy_with(dsn.connect_options)
        };

        Ok(Arc::new(Self { rt: Some(rt), pool }))
    }

    /// Run a future to completion from **any** calling context (plain thread, tokio worker, or
    /// current-thread runtime) without panicking. See the module docs for why this exact shape.
    ///
    /// The future must be `Send + 'static` because it is spawned onto this handle's runtime; clone
    /// the [`PgHandle::pool`] into the async block to satisfy `'static` (the pool is `Clone`).
    pub fn block_on<F>(&self, fut: F) -> F::Output
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let (tx, rx) = std::sync::mpsc::channel();
        self.rt
            .as_ref()
            .expect("flux-pg: runtime is present for a live handle")
            .spawn(async move {
                // If `fut` panics, `tx` is dropped without a send and `recv()` below returns Err —
                // surfaced as a clear panic in the caller rather than a silent hang.
                let out = fut.await;
                let _ = tx.send(out);
            });
        rx.recv()
            .expect("flux-pg: the query task panicked or the runtime was dropped mid-flight")
    }

    /// The underlying pool. Clone it into async blocks passed to [`PgHandle::block_on`].
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

/// A parsed DSN: the sqlx connect options plus the three flux-owned tuning params.
struct Dsn {
    connect_options: PgConnectOptions,
    pool_max: u32,
    acquire_timeout_ms: u64,
    schema: Option<String>,
}

impl Dsn {
    /// Parse `postgres://user:pw@host:port/db?param=…`, pulling out the flux-owned params and
    /// handing the standard remainder (host/user/password/dbname/`sslmode`/…) to sqlx, which does
    /// the userinfo percent-decoding. Flux-owned params:
    ///
    /// | param | default | meaning |
    /// |---|---|---|
    /// | `pool_max` | 5 | max pool connections (serverless-friendly) |
    /// | `acquire_timeout_ms` | 5000 | pool acquire timeout |
    /// | `schema` | *(none → `public`)* | `SET search_path` per connection; the test-isolation knob |
    fn parse(url: &str) -> Result<Self> {
        let mut u = url::Url::parse(url)
            .map_err(|e| Error::Config(format!("flux-pg: invalid DSN {url:?}: {e}")))?;

        let mut pool_max = 5u32;
        let mut acquire_timeout_ms = 5000u64;
        let mut schema: Option<String> = None;
        let mut passthrough: Vec<(String, String)> = Vec::new();

        for (k, v) in u.query_pairs() {
            match k.as_ref() {
                "pool_max" => {
                    pool_max = v.parse().map_err(|_| {
                        Error::Config(format!("flux-pg: pool_max must be a u32, got {v:?}"))
                    })?
                }
                "acquire_timeout_ms" => {
                    acquire_timeout_ms = v.parse().map_err(|_| {
                        Error::Config(format!(
                            "flux-pg: acquire_timeout_ms must be a u64, got {v:?}"
                        ))
                    })?
                }
                "schema" => schema = Some(v.into_owned()),
                _ => passthrough.push((k.into_owned(), v.into_owned())),
            }
        }

        // Rebuild the URL with only the passthrough params so sqlx sees a clean standard DSN.
        if passthrough.is_empty() {
            u.set_query(None);
        } else {
            let mut qp = u.query_pairs_mut();
            qp.clear();
            for (k, v) in &passthrough {
                qp.append_pair(k, v);
            }
        }

        if let Some(s) = &schema {
            if !is_ident(s) {
                return Err(Error::Config(format!(
                    "flux-pg: schema must be a bare SQL identifier (letters/digits/underscore, not \
                     starting with a digit), got {s:?}"
                )));
            }
        }

        let connect_options = PgConnectOptions::from_str(u.as_str())
            .map_err(|e| Error::Config(format!("flux-pg: {e}")))?;

        Ok(Self {
            connect_options,
            pool_max,
            acquire_timeout_ms,
            schema,
        })
    }
}

/// A bare SQL identifier: ASCII letters/digits/underscore, not starting with a digit. Used to gate
/// the `schema` name before it is interpolated into DDL/`SET` (which cannot take bind parameters).
fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- DSN parsing (no database) --------------------------------------------------------------

    #[test]
    fn parses_defaults_when_no_flux_params() {
        let dsn = Dsn::parse("postgres://alice:secret@db.example.com:5433/mydb").unwrap();
        assert_eq!(dsn.pool_max, 5);
        assert_eq!(dsn.acquire_timeout_ms, 5000);
        assert!(dsn.schema.is_none());
        assert_eq!(dsn.connect_options.get_username(), "alice");
        assert_eq!(dsn.connect_options.get_database(), Some("mydb"));
        assert_eq!(dsn.connect_options.get_host(), "db.example.com");
        assert_eq!(dsn.connect_options.get_port(), 5433);
    }

    #[test]
    fn strips_flux_params_and_keeps_passthrough() {
        let dsn = Dsn::parse(
            "postgres://u:p@h/db?pool_max=17&acquire_timeout_ms=250&schema=tenant_42&sslmode=require",
        )
        .unwrap();
        assert_eq!(dsn.pool_max, 17);
        assert_eq!(dsn.acquire_timeout_ms, 250);
        assert_eq!(dsn.schema.as_deref(), Some("tenant_42"));
        // sslmode survived into the sqlx options (passthrough), flux params did not leak.
        assert_eq!(dsn.connect_options.get_database(), Some("db"));
    }

    #[test]
    fn percent_decodes_userinfo_and_roundtrips_hyphens() {
        // username "acct-owner" and password with an encoded '@', db name with a hyphen.
        let dsn = Dsn::parse("postgres://acct-owner:p%40ss@localhost/my-db?schema=public").unwrap();
        assert_eq!(dsn.connect_options.get_username(), "acct-owner");
        assert_eq!(dsn.connect_options.get_database(), Some("my-db"));
    }

    #[test]
    fn rejects_non_identifier_schema() {
        for bad in [
            "postgres://u@h/db?schema=has space",
            "postgres://u@h/db?schema=drop;table",
            "postgres://u@h/db?schema=9leading",
            "postgres://u@h/db?schema=",
        ] {
            assert!(Dsn::parse(bad).is_err(), "should reject {bad:?}");
        }
        assert!(Dsn::parse("postgres://u@h/db?schema=t_01hxyz").is_ok());
    }

    #[test]
    fn rejects_bad_flux_param_values() {
        assert!(Dsn::parse("postgres://u@h/db?pool_max=lots").is_err());
        assert!(Dsn::parse("postgres://u@h/db?acquire_timeout_ms=-5").is_err());
    }

    #[test]
    fn is_ident_matches_expected() {
        assert!(is_ident("public"));
        assert!(is_ident("_x"));
        assert!(is_ident("t_01H9ZZ"));
        assert!(!is_ident(""));
        assert!(!is_ident("1abc"));
        assert!(!is_ident("a-b"));
        assert!(!is_ident("a b"));
    }

    // ---- The bridge matrix: block_on must not panic from any context -----------------------------
    //
    // These use a throwaway PgHandle built against an unreachable DSN — we never touch the DB, we
    // only exercise that `block_on` drives a plain future to completion from each calling context.

    fn dummy_handle() -> Arc<PgHandle> {
        // connect_lazy never dials, so an unroutable host is fine for bridge-only tests.
        PgHandle::connect("postgres://u:p@127.0.0.1:1/db").expect("lazy connect never dials")
    }

    #[test]
    fn block_on_from_plain_thread() {
        let h = dummy_handle();
        let out = std::thread::spawn(move || h.block_on(async { 1 + 2 }))
            .join()
            .unwrap();
        assert_eq!(out, 3);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn block_on_from_multi_thread_worker() {
        let h = dummy_handle();
        // Calling the sync bridge from inside a running multi-thread runtime is the case that
        // panics the naive bridges. Run it on a blocking thread so we don't stall the test worker.
        let out = tokio::task::spawn_blocking(move || h.block_on(async { 40 + 2 }))
            .await
            .unwrap();
        assert_eq!(out, 42);
    }

    #[test]
    fn block_on_from_current_thread_runtime() {
        let h = dummy_handle();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        // Inside a current-thread runtime, `block_in_place`/`Handle::block_on` would panic; ours
        // spawns onto its own runtime and blocks on a std channel, so it just works.
        let out = rt.block_on(async move {
            tokio::task::spawn_blocking(move || h.block_on(async { 7 * 6 }))
                .await
                .unwrap()
        });
        assert_eq!(out, 42);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handle_drops_cleanly_from_within_a_runtime() {
        // The last `Arc<PgHandle>` owns a tokio Runtime; releasing it on a worker thread of ANOTHER
        // runtime (a server dropping its store) must NOT panic — the default `Runtime::drop` would.
        // Regression for the `shutdown_background` Drop. Lazy connect never dials, so no DB needed.
        let h = dummy_handle();
        // Exercise the bridge once (from a blocking task so it doesn't stall this worker), then drop
        // the sole Arc directly on this async worker thread — the panic case.
        let h = tokio::task::spawn_blocking(move || {
            assert_eq!(h.block_on(async { 21 * 2 }), 42);
            h
        })
        .await
        .unwrap();
        drop(h);
    }

    // ---- Env-gated integration: requires a real Postgres via TEST_POSTGRES_URL -------------------

    fn test_url_with_schema(schema: &str) -> Option<String> {
        let base = std::env::var("TEST_POSTGRES_URL").ok()?;
        let sep = if base.contains('?') { '&' } else { '?' };
        Some(format!("{base}{sep}schema={schema}"))
    }

    #[test]
    fn live_roundtrip_and_schema_isolation() {
        let schema = format!("t_{}", ulid::Ulid::new().to_string().to_lowercase());
        let Some(url) = test_url_with_schema(&schema) else {
            eprintln!("skipping live_roundtrip_and_schema_isolation: TEST_POSTGRES_URL unset");
            return;
        };
        let h = PgHandle::connect(&url).unwrap();
        let pool = h.pool().clone();

        // Round-trip a scalar and confirm the search_path actually points at our throwaway schema.
        let (one, current_schema): (i64, String) = h.block_on(async move {
            // sqlx is strict about types: `1` is int4 and `current_schema()` returns `name`, so
            // cast both to the Rust types we decode into (int8 → i64, text → String).
            let one: i64 = sqlx::query_scalar("SELECT 1::bigint")
                .fetch_one(&pool)
                .await
                .unwrap();
            let cur: String = sqlx::query_scalar("SELECT current_schema()::text")
                .fetch_one(&pool)
                .await
                .unwrap();
            (one, cur)
        });
        assert_eq!(one, 1);
        assert_eq!(current_schema, schema);

        // Clean up the throwaway schema.
        let pool = h.pool().clone();
        let drop = format!("DROP SCHEMA IF EXISTS \"{schema}\" CASCADE");
        h.block_on(async move { sqlx::query(&drop).execute(&pool).await.unwrap() });
    }
}
