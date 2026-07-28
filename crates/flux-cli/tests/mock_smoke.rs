//! Gate-level guard for the offline `mock` provider: `flux run --yes -m mock` must route intent,
//! propose an action through the native schema, approve/execute its host-built batch, and write
//! `flux-mock.txt`. This runs the real binary end-to-end under an isolated HOME + CWD.

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

#[test]
fn default_mock_run_surfaces_intent_in_plain_output() {
    let tmp = TempDir::new("staged-intent-smoke");
    let work = tmp.path();
    let home = work.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_flux"))
        .args(["run", "--yes", "-m", "mock", "say hello"])
        .current_dir(work)
        .env("HOME", &home)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .output()
        .expect("spawn adaptive flux");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "adaptive mock run exited non-zero\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(stderr.contains("routing intent…"), "{stderr}");
    assert!(stderr.contains("exploring…"), "{stderr}");
    assert!(
        stderr.contains("◆ intent: complete the offline mock turn"),
        "{stderr}"
    );
    assert!(stderr.contains("capabilities: workspace.write"), "{stderr}");
    assert!(stderr.contains("operations"), "{stderr}");
}

/// C-162: a `[tools] disable` entry that matches no registered op is a loud startup warning naming
/// the entry, not a silent no-op — end-to-end through the real binary and a real `.flux/config.toml`.
#[test]
fn tools_disable_unmatched_entry_warns_at_startup() {
    let tmp = TempDir::new("tools-disable-unmatched");
    let work = tmp.path();
    let home = work.join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(work.join(".flux")).unwrap();
    std::fs::write(
        work.join(".flux/config.toml"),
        "[tools]\ndisable = [\"no-such-op\"]\n",
    )
    .unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_flux"))
        .args(["run", "--yes", "-m", "mock", "say hello"])
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
        "an unmatched disable entry must warn, not fail startup\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("[tools] disable entry `no-such-op` matches no known op"),
        "{stderr}"
    );
}

/// C-183: `flux app run` builds its own registry (separately from the interactive `build_agent_with`
/// path C-162 wired) — it must resolve `[tools] disable` too, including the same visible startup
/// warning for an unmatched entry, on the previously-unwired app-run assembly seam.
#[test]
fn app_run_tools_disable_unmatched_entry_warns_at_startup() {
    let tmp = TempDir::new("app-run-tools-disable-unmatched");
    let work = tmp.path();
    let home = work.join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(work.join(".flux")).unwrap();
    std::fs::write(
        work.join(".flux/config.toml"),
        "[tools]\ndisable = [\"no-such-op\"]\n",
    )
    .unwrap();
    std::fs::write(work.join("noop.flux"), "flow noop\n  return \"ok\"\n").unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_flux"))
        .args(["app", "run", "noop.flux", "--yes", "-m", "mock"])
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
        "an unmatched disable entry must warn, not fail `flux app run` startup\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("[tools] disable entry `no-such-op` matches no known op"),
        "{stderr}"
    );
}

#[cfg(unix)]
#[test]
fn malformed_config_stops_plugin_status_before_native_spawn() {
    use std::os::unix::fs::PermissionsExt as _;

    let tmp = TempDir::new("malformed-config-plugin-status");
    let work = tmp.path().join("work");
    let home = tmp.path().join("home");
    let plugins = home.join(".flux/plugins");
    std::fs::create_dir_all(work.join(".flux")).unwrap();
    std::fs::create_dir_all(&plugins).unwrap();

    // `require` is present, but the adjacent typo makes the deny-unknown-fields config fail as a
    // whole. Startup must not replace it with the default-off posture and continue to a native
    // plugin spawn.
    std::fs::write(
        work.join(".flux/config.toml"),
        "[sandbox]\nrequire = true\nenabeld = true\n",
    )
    .unwrap();
    let marker = tmp.path().join("plugin-was-spawned");
    let plugin = tmp.path().join("probe-plugin.sh");
    std::fs::write(
        &plugin,
        format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()),
    )
    .unwrap();
    let mut perms = std::fs::metadata(&plugin).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&plugin, perms).unwrap();
    std::fs::write(
        plugins.join("probe.toml"),
        format!("program = {:?}\nargs = []\n", plugin.display().to_string()),
    )
    .unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_flux"))
        .args(["plugin", "status", "probe"])
        .current_dir(&work)
        .env("HOME", &home)
        .env("NO_COLOR", "1")
        .env_remove("FLUX_SANDBOX")
        .env_remove("FLUX_SANDBOXED")
        .stdin(Stdio::null())
        .output()
        .expect("spawn flux");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "malformed security config must stop startup: {stderr}"
    );
    assert!(stderr.contains("config.toml"), "{stderr}");
    assert!(
        !marker.exists(),
        "plugin executed despite malformed config with a requested fail-closed posture"
    );
}
