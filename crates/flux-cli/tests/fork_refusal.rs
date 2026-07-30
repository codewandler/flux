//! C-211: `flux fork` refuses a parent whose log ends mid-tool-pair, and the refusal costs nothing.
//!
//! The CLI carries its own hand-written copy of validate-then-rewrite (`session.rs`), with different
//! error plumbing (`with_context` + `anyhow!` rather than `?` through `flux_core::Error`) than
//! [`flux_sdk::Session::fork`]. The SDK's tests therefore do not cover it, and a divergence between
//! the two would surface as a CLI that still mints broken children. This drives the real binary.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use flux_events::EventStore;

/// A temp dir that removes itself on drop — so a failing assertion (a panic) can't leak it.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "flux-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("create temp dir");
        TempDir(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Seed `<store>/events.db` with one session whose conversation ends on an unanswered `tool_use` —
/// exactly what a crashed turn leaves behind, and the shape no provider will accept. Written
/// through the raw append seam because the typed session log is what refuses it. Returns the id.
///
/// The store is closed before returning so the spawned binary opens it cleanly.
fn seed_unforkable_session(store: &Path) -> String {
    std::fs::create_dir_all(store).unwrap();
    let events = EventStore::open(store.join("events.db")).unwrap();
    let sid = events.create_session("mock").unwrap();
    events
        .append(
            &sid,
            flux_events::NewEvent::message(flux_core::Message::user_text("read the note")),
        )
        .unwrap();
    events
        .append(
            &sid,
            flux_events::NewEvent::message(flux_core::Message::assistant(vec![
                flux_core::ContentBlock::ToolUse {
                    id: "orphan-1".into(),
                    name: "read".into(),
                    input: serde_json::json!({}),
                },
            ])),
        )
        .unwrap();
    sid
}

/// Count the sessions in `<store>/events.db`. Generous limit — this fixture holds one.
fn session_count(store: &Path) -> usize {
    EventStore::open(store.join("events.db"))
        .unwrap()
        .list(1_000)
        .unwrap()
        .len()
}

/// **Failing-first (C-211)**: the CLI fork's refusal branch, which had no test at all. It must
/// refuse the unforkable parent *and* leave no child session behind — before this story it minted
/// the child before validating, so the count grew by one on the way to the error.
#[test]
fn cli_fork_refuses_an_unforkable_parent_without_minting_a_child() {
    let tmp = TempDir::new("fork-refusal");
    let work = tmp.path();
    let home = work.join("home");
    let store = work.join("store");
    std::fs::create_dir_all(&home).unwrap();

    let sid = seed_unforkable_session(&store);
    let before = session_count(&store);
    assert_eq!(before, 1, "the fixture starts with exactly one session");

    let out = Command::new(env!("CARGO_BIN_EXE_flux"))
        .args(["fork", &sid, "--at", "0", "-m", "mock"])
        .arg("--store")
        .arg(&store)
        .current_dir(work)
        .env("HOME", &home)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .output()
        .expect("spawn flux");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "`flux fork` of a mid-tool-pair history must fail\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("cannot be forked"),
        "the refusal must name the session it refused\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("tool_use") && stderr.contains("orphan-1"),
        "the refusal must name the invariant it protects\nstderr: {stderr}"
    );

    assert_eq!(
        session_count(&store),
        before,
        "a refused fork must not leave a child session behind\nstderr: {stderr}"
    );
    // `tmp` cleans itself up on drop (including on an earlier panic).
}
