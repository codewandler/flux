//! C-41 / C-18: A2A session TTL pruning exercised through the real HTTP surface (not the
//! `router_with_ttl` white-box seam the crate's own inline tests use — this suite only sees
//! `flux_server`'s public API).
//!
//! `flux_server::router` resolves the TTL from `[server] a2a_session_ttl_secs` in the layered
//! `flux-config` (`crates/flux-config/src/lib.rs`), read from the **process** current directory at
//! router-build time (`a2a_ttl_from_config` in `crates/flux-server/src/lib.rs`). That is the only
//! knob reachable from outside the crate (the `A2aTtl`/`router_with_ttl` seam the inline tests use
//! is `pub(crate)`), so this test drives it the same way the production CLI does: write a project
//! `.flux/config.toml` and point the process at that directory for the duration of the test. A
//! process-wide `Mutex` serializes the `set_current_dir` critical section (cwd is global process
//! state) and an RAII guard restores the original directory even on panic.
//!
//! ## The C-29 queued-session property — not independently reachable here
//!
//! C-18's Acceptance also names the C-29 guarantee ("a session queued behind another in-flight
//! turn is never pruned by a concurrent mint's sweep"). That property is **not** reachable through
//! black-box HTTP against a single router/server instance: `send`/`subscribe` acquire the
//! single-turn `turn_gate` and hold it for the mint *and* the full run (see `a2a.rs`'s
//! `create_a2a_session` doc comment), so within one `Router`, a second request's mint can only ever
//! begin strictly after the first request's turn has fully finished — there is no window in which
//! two mints (and so two sweeps) are actually concurrent. Reproducing the hazard requires calling
//! the mint function directly, ahead of / independent from the gate, which is exactly what the
//! crate's own `a2a::tests::queued_session_survives_concurrent_sweep_while_gate_held` does with
//! white-box access to `create_a2a_session`/`TurnGate` (both `pub(crate)`, invisible here). Per the
//! C-41 story's brief, this is recorded honestly rather than forced: that inline test remains the
//! regression pin for C-29, and it isn't duplicated (or weakened) by anything in this suite.

mod support;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use serde_json::json;

/// Serializes every test in this binary that touches the process's current directory — `cwd` is
/// global process state, and cargo's built-in test harness otherwise runs `#[tokio::test]`
/// functions in this file concurrently on different OS threads.
static CWD_LOCK: Mutex<()> = Mutex::new(());

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Enters `dir` as the process cwd for the lifetime of the returned guard, holding `CWD_LOCK` the
/// whole time; restores the original cwd on drop (including on panic/unwind).
struct CwdGuard {
    original: PathBuf,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl CwdGuard {
    fn enter(dir: &Path) -> Self {
        let lock = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let original = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(dir).expect("set current dir");
        CwdGuard {
            original,
            _lock: lock,
        }
    }
}

impl Drop for CwdGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.original);
    }
}

/// A fresh project directory carrying `.flux/config.toml` with `[server] a2a_session_ttl_secs =
/// <ttl_secs>` — what `flux_server::router`'s `a2a_ttl_from_config` reads from the process cwd.
fn project_dir_with_ttl(ttl_secs: u64) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "flux-server-it-ttl-project-{}-{n}",
        std::process::id()
    ));
    let flux_dir = dir.join(".flux");
    std::fs::create_dir_all(&flux_dir).unwrap();
    std::fs::write(
        flux_dir.join("config.toml"),
        format!("[server]\na2a_session_ttl_secs = {ttl_secs}\n"),
    )
    .unwrap();
    dir
}

/// Send `message/send` with the given `contextId` and return the minted session (task) id.
async fn send_and_get_task_id(app: axum::Router, context_id: &str) -> String {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "message/send",
        "params": {
            "message": {
                "contextId": context_id,
                "parts": [{ "kind": "text", "text": "hi" }],
            },
            // Blocking: the TTL assertions need the turn finished when the response returns.
            "configuration": { "blocking": true },
        }
    });
    let (status, res) = support::post_json(app, "/a2a", body).await;
    assert_eq!(status, StatusCode::OK, "unexpected response: {res}");
    assert!(res.get("error").is_none(), "unexpected error: {res}");
    res["result"]["id"]
        .as_str()
        .expect("message/send returns a Task with an id")
        .to_string()
}

/// C-18 through the HTTP surface: a session minted by an earlier `message/send` and then left to
/// age past the configured TTL is swept away by the *next* `message/send`'s mint-time sweep — the
/// same lazy-sweep-at-mint mechanism `a2a.rs::create_a2a_session` documents, driven here purely
/// through `POST /a2a` and the production `[server] a2a_session_ttl_secs` config knob (no access to
/// the crate's private `A2aTtl`/`router_with_ttl` test seam).
#[tokio::test]
async fn expired_a2a_session_is_swept_at_the_next_mint() {
    let project = project_dir_with_ttl(1); // 1s TTL — small enough to cross for real in a test.
    let _cwd = CwdGuard::enter(&project);

    let engine = support::test_engine(Arc::new(support::ProseProvider));
    let app = flux_server::router(
        engine.clone(),
        flux_server::ServerAuth::Open,
        flux_server::CardInfo::flux_coding(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .unwrap();

    // Mint session A.
    let session_a = send_and_get_task_id(app.clone(), "ctx-a").await;
    assert!(
        engine.events.info(&session_a).is_ok(),
        "session A exists right after mint"
    );

    // Age it past the 1s TTL for real.
    tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;

    // Mint session B — its mint runs the lazy TTL sweep first, using the real wall clock, so
    // session A (now well past its 1s TTL, untouched since) is swept.
    let session_b = send_and_get_task_id(app, "ctx-b").await;

    assert!(
        engine.events.info(&session_a).is_err(),
        "session A is past the TTL and must be pruned by session B's mint-time sweep"
    );
    assert!(
        engine.events.info(&session_b).is_ok(),
        "session B was JUST minted — nowhere near the TTL — and must survive its own sweep pass"
    );
}

/// `a2a_session_ttl_secs = 0` disables pruning even through the HTTP surface — an old session
/// survives every subsequent mint.
#[tokio::test]
async fn ttl_zero_disables_pruning_through_http() {
    let project = project_dir_with_ttl(0);
    let _cwd = CwdGuard::enter(&project);

    let engine = support::test_engine(Arc::new(support::ProseProvider));
    let app = flux_server::router(
        engine.clone(),
        flux_server::ServerAuth::Open,
        flux_server::CardInfo::flux_coding(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .unwrap();

    let session_a = send_and_get_task_id(app.clone(), "ctx-a").await;
    tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
    let _session_b = send_and_get_task_id(app, "ctx-b").await;

    assert!(
        engine.events.info(&session_a).is_ok(),
        "ttl = 0 means never prune, however stale a session gets"
    );
}
