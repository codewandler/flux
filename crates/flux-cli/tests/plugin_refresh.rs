//! C-310 — `flux plugin refresh <name>` must be a reachable operator surface.
//!
//! The catalog-refresh *behavior* is exercised against a manifest-drifting fixture plugin in
//! `codewandler-flux-plugin`'s `catalog_refresh.rs` (that fixture binary is not built for this
//! crate's tests). What this file locks in is the CLI seam: the subcommand parses, reaches the
//! plugin_cmd handler, and fails on the plugin rather than on the argument grammar.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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

#[test]
fn plugin_refresh_is_a_reachable_subcommand() {
    let tmp = TempDir::new("plugin-refresh");
    let work = tmp.path();
    let home = work.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_flux"))
        .args(["plugin", "refresh", "no-such-plugin"])
        .current_dir(work)
        .env("HOME", &home)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .output()
        .expect("spawn flux");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let combined = format!("{stdout}{stderr}");

    // The grammar must know the verb — this is the assertion that fails before the subcommand
    // exists, with clap's `unrecognized subcommand 'refresh'`.
    assert!(
        !combined.contains("unrecognized subcommand"),
        "`flux plugin refresh` is not a known subcommand:\n{combined}"
    );
    // And it must reach the handler: the failure is about the plugin, not about the arguments.
    assert!(
        combined.contains("no such plugin") && combined.contains("no-such-plugin"),
        "`flux plugin refresh` did not reach the plugin handler:\n{combined}"
    );
    assert!(
        !out.status.success(),
        "refreshing an uninstalled plugin must not exit 0:\n{combined}"
    );
}
