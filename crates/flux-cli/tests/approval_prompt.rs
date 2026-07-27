//! C-91: without `--yes` the plain CLI must *show* the approval prompt (it used to be erased by the
//! stderr spinner) and the prompt must carry the batch content. Runs the real binary end-to-end
//! against the offline `mock` provider under an isolated HOME + CWD, answering via piped stdin.

use std::io::Write as _;
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

/// Run `flux run -m mock` (NO `--yes`) in an isolated dir, feeding `answer` on piped stdin.
/// Returns (status_ok, stdout, stderr, workdir kept alive by the caller's TempDir).
fn run_mock_with_answer(work: &Path, answer: &str) -> (bool, String, String) {
    let home = work.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_flux"))
        .args(["run", "-m", "mock", "write a quick note"])
        .current_dir(work)
        .env("HOME", &home)
        .env("NO_COLOR", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn flux");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(answer.as_bytes())
        .expect("write answer");
    let out = child.wait_with_output().expect("wait for flux");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn plan_approval_prompt_is_visible_with_piped_stdin() {
    let tmp = TempDir::new("approval-prompt-yes");
    let work = tmp.path();
    let (ok, stdout, stderr) = run_mock_with_answer(work, "y\n");

    assert!(
        ok,
        "`flux run -m mock` (no --yes, answered y) exited non-zero\nstdout: {stdout}\nstderr: {stderr}"
    );
    // The prompt itself must be visible on stderr…
    assert!(
        stderr.contains("run this plan?"),
        "approval prompt missing from stderr: {stderr}"
    );
    assert!(
        stderr.contains("[y]es / [a]lways / [N]o:"),
        "answer line missing from stderr: {stderr}"
    );
    // …and must say WHAT is being approved (the plain CLI renders no plan tree beforehand).
    assert!(
        stderr.contains("ops: append"),
        "batch content missing from the prompt: {stderr}"
    );
    assert!(
        stderr.contains("flux-mock.txt"),
        "write target missing from the prompt: {stderr}"
    );
    // The piped `y` was consumed as the approval: the batch executed.
    assert!(
        work.join("flux-mock.txt").exists(),
        "flux-mock.txt was not written after approval\nstdout: {stdout}\nstderr: {stderr}"
    );
}

#[test]
fn plan_approval_denied_on_n() {
    let tmp = TempDir::new("approval-prompt-no");
    let work = tmp.path();
    let (_ok, stdout, stderr) = run_mock_with_answer(work, "n\n");

    assert!(
        stderr.contains("run this plan?"),
        "approval prompt missing from stderr: {stderr}"
    );
    assert!(
        !work.join("flux-mock.txt").exists(),
        "flux-mock.txt written despite a denied plan\nstdout: {stdout}\nstderr: {stderr}"
    );
}
