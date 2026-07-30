//! C-274: the SQLite driver must be a *feature*, not a fact, for the engine's dependency graph.
//!
//! The acceptance evidence for that story is a `cargo tree` result — with the store's SQLite backend
//! off, `cargo tree -p codewandler-flux-flow -i rusqlite` must report **no** path. That command
//! cannot run inside a unit test (a nested `cargo` invocation against the same target directory is
//! neither fast nor reliable), so this test asserts the manifest facts the tree result is derived
//! from, on the two crates that name the driver directly:
//!
//! 1. `rusqlite` is an **optional** dependency of both `flux-events` and `flux-flow`, gated behind a
//!    `sqlite` feature that is **on by default** — so no existing consumer changes behaviour or
//!    gains a build step.
//! 2. The workspace's `flux-events` entry sets `default-features = false`, and `flux-flow` re-enables
//!    the backend through its own `sqlite` feature. Without both halves, disabling flux-flow's
//!    feature would leave flux-events' default on and the driver would still reach the engine by the
//!    second path — the trap C-270 fell into by reading one manifest line instead of the tree. It has
//!    to be the *workspace* entry because a member's own `default-features = false` is silently
//!    ignored for a workspace-inherited dependency.
//!
//! Manifest-shaped rather than behavioural on purpose: the thing being guarded *is* a manifest fact,
//! and a build that links a C library cannot observe its own absence from inside itself.

use std::path::{Path, PathBuf};

/// The workspace root — two levels up from this crate's directory.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/<crate> sits two levels below the workspace root")
        .to_path_buf()
}

fn manifest(crate_dir: &str) -> String {
    let path = repo_root()
        .join("crates")
        .join(crate_dir)
        .join("Cargo.toml");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The body of a `[section]` in `text`, up to the next section header.
fn section(text: &str, header: &str) -> String {
    text.lines()
        .skip_while(|l| l.trim() != header)
        .skip(1)
        .take_while(|l| !l.trim_start().starts_with('['))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The `[dependencies]` entry for `name` — the whole line, whichever form it takes
/// (`name.workspace = true`, `name = { … }`).
fn dep_entry(text: &str, name: &str) -> String {
    let deps = section(text, "[dependencies]");
    deps.lines()
        .find(|l| {
            let l = l.trim_start();
            l.starts_with(&format!("{name} ")) || l.starts_with(&format!("{name}."))
        })
        .unwrap_or_else(|| panic!("no [dependencies] entry for {name}"))
        .to_string()
}

/// Both drivers-bearing crates gate `rusqlite` behind a default-on `sqlite` feature.
#[test]
fn rusqlite_is_optional_and_default_on_in_the_crates_that_name_it() {
    for crate_dir in ["flux-events", "flux-flow"] {
        let text = manifest(crate_dir);

        let entry = dep_entry(&text, "rusqlite");
        assert!(
            entry.contains("optional = true"),
            "{crate_dir}: rusqlite must be an optional dependency so a wasm32 build can drop it, \
             got: {entry}"
        );

        let features = section(&text, "[features]");
        let default = features
            .lines()
            .find(|l| l.trim_start().starts_with("default ="))
            .unwrap_or_else(|| panic!("{crate_dir}: no `default = [...]` feature list"))
            .to_string();
        assert!(
            default.contains("\"sqlite\""),
            "{crate_dir}: the `sqlite` feature must be ON by default, so no existing consumer \
             changes behaviour or gains a build step, got: {default}"
        );

        let sqlite = features
            .lines()
            .find(|l| l.trim_start().starts_with("sqlite ="))
            .unwrap_or_else(|| panic!("{crate_dir}: no `sqlite = [...]` feature"))
            .to_string();
        assert!(
            sqlite.contains("dep:rusqlite"),
            "{crate_dir}: the `sqlite` feature must be what pulls the driver in, got: {sqlite}"
        );
    }
}

/// The second path the driver reaches the engine by: flux-events' own default features must not
/// re-enable it behind flux-flow's back.
#[test]
fn flux_flow_takes_flux_events_without_its_default_backend() {
    let workspace = std::fs::read_to_string(repo_root().join("Cargo.toml")).expect("root manifest");
    let entry = workspace
        .lines()
        .find(|l| l.starts_with("flux-events = "))
        .expect("a [workspace.dependencies] entry for flux-events");
    assert!(
        entry.contains("default-features = false"),
        "the WORKSPACE flux-events entry must set default-features = false (a member's own is \
         silently ignored for an inherited dependency) — otherwise `--no-default-features` on \
         flux-flow still links rusqlite through flux-events, got: {entry}"
    );

    let text = manifest("flux-flow");
    let flow_entry = dep_entry(&text, "flux-events");
    assert!(
        !flow_entry.contains("sqlite"),
        "flux-flow must NOT opt its flux-events dependency into the backend directly — that is what \
         its own default-on `sqlite` feature is for, got: {flow_entry}"
    );

    let sqlite = section(&text, "[features]")
        .lines()
        .find(|l| l.trim_start().starts_with("sqlite ="))
        .unwrap_or_else(|| panic!("flux-flow: no `sqlite = [...]` feature"))
        .to_string();
    assert!(
        sqlite.contains("flux-events/sqlite"),
        "flux-flow's `sqlite` feature must re-enable the store backend it just switched off, \
         got: {sqlite}"
    );
}
