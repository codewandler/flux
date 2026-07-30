//! C-269: the guarded-IO port is a real seam.
//!
//! Before the port existed, [`flux_plugin::SystemSource`] handed back a concrete
//! `Arc<flux_system::System>`, so the plugin host's `process.run` / `fs.read` capabilities could
//! only ever be served by the native syscall backend. These tests substitute a backend that starts
//! no OS process and opens no file — the shape the portable-Wasm epic needs (host imports instead
//! of syscalls) — and drive it through the *unmodified* host-capability path.
//!
//! They also pin the fail-closed half of the contract: the port's optional operations
//! (`run_with_stdin`, `spawn_background`) deny by default, so a substrate that cannot host them
//! refuses rather than silently degrading.

use std::sync::Arc;
use std::time::Duration;

use flux_plugin::{
    HostCapabilities, PluginCapabilities, PluginSystem, SystemHostCaps, SystemSource,
};
use flux_system::port::{Guarded, GuardedEnv, GuardedHostFiles, GuardedProcess, Result};
use flux_system::{ProcessOutput, ScopedFileRead};
use serde_json::json;

/// A guarded substrate with no ambient authority at all: `run_with_env` answers from a canned table
/// and every other operation is left at the port's fail-closed default. Stands in for a host-import
/// backend (Wasm embedder, remote executor) — the point is that nothing here is a syscall.
struct TabledSystem {
    /// The single canned answer, returned for whatever argv is asked for.
    answer: ProcessOutput,
}

impl GuardedProcess for TabledSystem {
    fn run_with_env<'a>(
        &'a self,
        argv: &'a [String],
        _env: &'a [(String, String)],
        _timeout: Duration,
    ) -> Guarded<'a, ProcessOutput> {
        let mut answer = self.answer.clone();
        answer.stdout = format!("{}: {}", argv.join(" "), answer.stdout);
        Box::pin(async move { Ok(answer) })
    }
}

impl GuardedEnv for TabledSystem {
    fn env(&self, _key: &str) -> Option<String> {
        // No process environment exists on this substrate; a credential ref therefore cannot resolve.
        None
    }
}

impl GuardedHostFiles for TabledSystem {
    fn host_path_identity(&self, path: &str) -> Result<String> {
        Ok(path.to_string())
    }

    fn read_file_scoped<'a>(
        &'a self,
        path: &'a str,
        _scope: &'a str,
        _max_bytes: usize,
    ) -> Guarded<'a, ScopedFileRead> {
        let bytes = format!("contents of {path}").into_bytes();
        Box::pin(async move {
            Ok(ScopedFileRead {
                size: bytes.len() as u64,
                truncated: false,
                bytes,
            })
        })
    }
}

/// A [`SystemSource`] over the non-native substrate — the same seam the CLI's workspace adapter uses.
struct TabledSource(Arc<TabledSystem>);

impl SystemSource for TabledSource {
    fn system(&self) -> Arc<dyn PluginSystem> {
        self.0.clone()
    }
}

fn caps(answer: ProcessOutput) -> SystemHostCaps {
    SystemHostCaps::from_source(Arc::new(TabledSource(Arc::new(TabledSystem { answer }))))
        .with_grants(PluginCapabilities {
            process: vec!["true".into()],
            ..PluginCapabilities::default()
        })
}

fn output(stdout: &str) -> ProcessOutput {
    ProcessOutput {
        stdout: stdout.to_string(),
        stderr: String::new(),
        exit_code: 0,
    }
}

/// The seam itself: a consumer that previously required the concrete `System` accepts a backend
/// that never touches the OS, and its guarded `process.run` capability answers from that backend.
#[tokio::test]
async fn a_non_native_substrate_serves_the_plugin_host_process_capability() {
    let caps = caps(output("from the port"));

    let result = caps
        .handle("process.run", &json!({ "argv": ["true"] }))
        .await
        .expect("process.run served by the non-native substrate");

    assert_eq!(
        result.get("stdout").and_then(|v| v.as_str()),
        Some("true: from the port"),
        "the canned answer must come from the substituted backend, not a spawned process"
    );
    assert_eq!(result.get("exit_code").and_then(|v| v.as_i64()), Some(0));
}

/// The manifest gate is unchanged by the substitution: an ungranted argv is still refused before the
/// backend is consulted, so the seam cannot be used to route around the capability check.
#[tokio::test]
async fn the_manifest_process_gate_still_applies_over_a_non_native_substrate() {
    let caps = caps(output("from the port"));

    let denied = caps
        .handle("process.run", &json!({ "argv": ["rm", "-rf", "/"] }))
        .await
        .expect_err("an ungranted argv must be refused");

    assert!(
        denied.contains("granted process capabilities"),
        "expected the manifest argv-prefix denial, got: {denied}"
    );
}

/// The port's optional operations fail closed. `process.spawn` needs a live native child handle,
/// which a non-syscall substrate cannot produce, so leaving the default must deny.
#[tokio::test]
async fn optional_port_operations_deny_by_default_on_a_non_native_substrate() {
    let caps = caps(output("from the port"));

    let denied = caps
        .handle("process.spawn", &json!({ "argv": ["true"] }))
        .await
        .expect_err("a substrate with no OS processes must refuse process.spawn");

    assert!(
        denied.contains("cannot host long-lived child processes"),
        "expected the fail-closed port default, got: {denied}"
    );
}
