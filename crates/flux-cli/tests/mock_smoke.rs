//! Gate-level guard for the offline `mock` provider (F1): `flux run --yes -m mock` must actually
//! execute its canned plan and write `flux-mock.txt`. The loop-unit tests drive the agent loop with
//! a *scripted* provider, so they cannot catch a stale `MockCliProvider` AST that plan validation
//! rejects — and a rejected plan is silently repaired into a prose "Finished." with a **zero exit**,
//! which is exactly how this regressed unnoticed. This runs the real binary end-to-end under an
//! isolated HOME + CWD, so the observable "did it write the file" contract is enforced by the gate.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A temp dir that removes itself on drop — so a failing assertion (a panic) can't leak it. `Drop`
/// runs on unwind, unlike a cleanup statement at the end of the test.
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

#[test]
fn mock_run_writes_flux_mock_file() {
    let tmp = TempDir::new("mock-smoke");
    let work = tmp.path();
    let home = work.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_flux"))
        .args(["run", "--yes", "-m", "mock", "write a quick note"])
        .current_dir(work)
        .env("HOME", &home)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .output()
        .expect("spawn flux");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "`flux run --yes -m mock` exited non-zero\nstdout: {stdout}\nstderr: {stderr}"
    );

    let file = work.join("flux-mock.txt");
    let content = std::fs::read_to_string(&file).unwrap_or_else(|e| {
        panic!("flux-mock.txt was not written ({e})\nstdout: {stdout}\nstderr: {stderr}")
    });
    assert!(
        content.contains("created by flux mock"),
        "unexpected flux-mock.txt content: {content:?}"
    );
    // `tmp` cleans itself up on drop (including on an earlier panic).
}
