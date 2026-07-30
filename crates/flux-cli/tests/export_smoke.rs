//! Golden test for C-132's `flux export <run> -o run.html`: a recorded offline `-m mock` run
//! exports to a single self-contained static HTML file — the plan tree, per-op result/diff, cost,
//! and timeline — with no network references and no script. Runs the real binary end-to-end under
//! an isolated HOME + CWD, mirroring `mock_smoke.rs`'s pattern.
//!
//! Spawns set `FLUX_SANDBOX=off`: C-262 makes auto-approved non-interactive surfaces fail closed
//! without an OS sandbox backend, which no stock CI runner has. Confinement posture is asserted in
//! `sandbox_posture.rs`, not here — do not remove it, or this file only passes where `bwrap` exists.

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
fn export_renders_a_recorded_mock_run_as_one_self_contained_html_file() {
    let tmp = TempDir::new("export-smoke");
    let work = tmp.path();
    let home = work.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // Record a run: the scripted `-m mock` provider writes flux-mock.txt (a real `write` op, so the
    // recorded run has a real op result + diff to export).
    let run = Command::new(env!("CARGO_BIN_EXE_flux"))
        .args(["run", "--yes", "-m", "mock", "write a quick note"])
        .current_dir(work)
        .env("HOME", &home)
        .env("NO_COLOR", "1")
        .env("FLUX_SANDBOX", "off")
        .stdin(Stdio::null())
        .output()
        .expect("spawn flux run");
    assert!(
        run.status.success(),
        "recording run failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    // Export it — into a nested, not-yet-existing directory, so parent creation is covered too.
    let out_path = work.join("artifacts/run.html");
    let export = Command::new(env!("CARGO_BIN_EXE_flux"))
        .args(["export", "last", "-o", out_path.to_str().unwrap()])
        .current_dir(work)
        .env("HOME", &home)
        .env("NO_COLOR", "1")
        .env("FLUX_SANDBOX", "off")
        .stdin(Stdio::null())
        .output()
        .expect("spawn flux export");
    let export_stdout = String::from_utf8_lossy(&export.stdout);
    let export_stderr = String::from_utf8_lossy(&export.stderr);
    assert!(
        export.status.success(),
        "flux export exited non-zero\nstdout: {export_stdout}\nstderr: {export_stderr}"
    );

    let html = std::fs::read_to_string(&out_path).unwrap_or_else(|e| {
        panic!("run.html was not written ({e})\nstdout: {export_stdout}\nstderr: {export_stderr}")
    });

    // Single self-contained document.
    assert!(html.starts_with("<!doctype html>"), "{html}");
    assert!(html.trim_end().ends_with("</html>"), "{html}");
    assert!(!html.contains("<script"), "must ship with no JS:\n{html}");
    assert!(
        !html.contains("<link"),
        "must inline CSS, not link it:\n{html}"
    );
    assert!(
        !html.contains("http://") && !html.contains("https://"),
        "must reference no network resource:\n{html}"
    );

    // The plan tree — reusing `render_styled_spans`, so the `flow` keyword is present as a styled
    // span (the exact same substrate `flow_render`'s SVG tree view uses).
    assert!(html.contains("class=\"plan-tree\""), "{html}");
    assert!(
        html.contains("tok-kw"),
        "no styled plan-tree spans:\n{html}"
    );

    // Per-op result for the recorded file-writing op (the scripted mock's family match for
    // "write" lands on the lower-risk `append` tool — see `export_cmd::tests` for a dedicated
    // `write`/diff-rendering test that doesn't depend on that heuristic).
    assert!(html.contains("flux-mock.txt"), "{html}");
    assert!(html.contains("class=\"op op-ok\""), "{html}");
    assert!(
        html.contains("appended 21 bytes to flux-mock.txt"),
        "{html}"
    );

    // Cost + timeline sections both render (structure only — mock usage varies).
    assert!(html.contains("<h4>Cost</h4>"), "{html}");
    assert!(html.contains("<h4>Timeline</h4>"), "{html}");
    assert!(
        html.contains("write a quick note"),
        "prompt missing from timeline:\n{html}"
    );

    // Pure read + deterministic rendering: `flux export` never mutates the recording it just read
    // (no live "generated at" timestamp either), so a second export — this time to stdout, the
    // no-`-o` convention `flux render` also uses — reproduces the file byte-for-byte.
    let export2 = Command::new(env!("CARGO_BIN_EXE_flux"))
        .args(["export", "last"])
        .current_dir(work)
        .env("HOME", &home)
        .env("NO_COLOR", "1")
        .env("FLUX_SANDBOX", "off")
        .stdin(Stdio::null())
        .output()
        .expect("spawn flux export (stdout)");
    assert!(export2.status.success());
    let stdout_html = String::from_utf8_lossy(&export2.stdout);
    assert_eq!(stdout_html.trim_end(), html.trim_end());
}
