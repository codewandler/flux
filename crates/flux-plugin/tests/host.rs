//! End-to-end: the host spawns plugin subprocesses and drives them over the framed stdio protocol
//! (manifest discovery, operation calls, host-capability callbacks, and tool projection).

use std::sync::Arc;

use async_trait::async_trait;
use flux_plugin::{HostCapabilities, PluginHost};
use serde_json::{json, Value};

/// A throwaway workspace-rooted `System` for spawning plugins in tests. The plugin launches through
/// flux's one guarded spawn path, which needs a `System`; the workspace dir is irrelevant to these
/// protocol/manifest tests (the echo/caps plugins do no file IO of their own).
fn test_system() -> flux_system::System {
    flux_system::System::new(flux_system::Workspace::new(std::env::temp_dir()).unwrap())
}

#[tokio::test]
async fn host_discovers_manifest_and_calls_operation() {
    let exe = env!("CARGO_BIN_EXE_echo_plugin");
    let system = test_system();
    let mut host = PluginHost::spawn(&system, exe, &[]).await.unwrap();

    let manifest = host.manifest().await.unwrap();
    assert_eq!(manifest.name, "echo");
    assert!(manifest.operations.iter().any(|o| o.name == "upper"));

    let out = host
        .call("upper", json!({"text": "hello plugin"}))
        .await
        .unwrap();
    assert_eq!(out["text"], "HELLO PLUGIN");

    // unknown operation surfaces as an error
    assert!(host.call("nope", json!({})).await.is_err());

    host.shutdown().await.unwrap();
}

/// A test host capability: answers `ping` by echoing the payload back.
struct PingCaps;

#[async_trait]
impl HostCapabilities for PingCaps {
    async fn handle(&self, command: &str, payload: &Value) -> Result<Value, String> {
        if command == "ping" {
            Ok(json!({ "pong": payload.get("echo").cloned().unwrap_or(Value::Null) }))
        } else {
            Err(format!("unknown capability {command}"))
        }
    }
}

#[tokio::test]
async fn host_services_plugin_capability_callback() {
    let exe = env!("CARGO_BIN_EXE_caps_plugin");
    let system = test_system();
    let mut host = PluginHost::spawn(&system, exe, &[]).await.unwrap();

    // The plugin's `viahost` op calls back into the host (`ping`); the round-trip returns the echo.
    let out = host
        .call_with_host("viahost", json!({"msg": "round-trip"}), &PingCaps)
        .await
        .unwrap();
    assert_eq!(out["host_said"]["pong"], "round-trip");

    // Without host capabilities, the same callback is denied.
    let denied = host.call("viahost", json!({"msg": "x"})).await;
    assert!(denied.is_err());

    host.shutdown().await.unwrap();
}

struct BlockingCaps {
    entered: tokio::sync::Semaphore,
}

#[async_trait]
impl HostCapabilities for BlockingCaps {
    async fn handle(&self, _command: &str, _payload: &Value) -> Result<Value, String> {
        self.entered.add_permits(1);
        std::future::pending().await
    }
}

/// Cancelling an operation while its plugin is blocked in a host callback must not leave the
/// callback frame half-consumed. The next dispatch either owns a clean protocol session or restarts
/// one; it must never feed an operation request to the guest's callback-response reader and hang.
#[tokio::test]
async fn cancellation_during_callback_does_not_desynchronize_next_dispatch() {
    let exe = env!("CARGO_BIN_EXE_caps_plugin");
    let system = test_system();
    let host = Arc::new(tokio::sync::Mutex::new(
        PluginHost::spawn(&system, exe, &[]).await.unwrap(),
    ));
    let blocking = Arc::new(BlockingCaps {
        entered: tokio::sync::Semaphore::new(0),
    });

    let cancelled = tokio::spawn({
        let host = host.clone();
        let blocking = blocking.clone();
        async move {
            host.lock()
                .await
                .call_with_host("viahost", json!({"msg": "cancel me"}), blocking.as_ref())
                .await
        }
    });
    blocking
        .entered
        .acquire()
        .await
        .expect("callback entered")
        .forget();
    cancelled.abort();
    assert!(cancelled.await.unwrap_err().is_cancelled());

    let next = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        host.lock()
            .await
            .call_with_host("viahost", json!({"msg": "after cancel"}), &PingCaps)
            .await
    })
    .await
    .expect("next dispatch must not deadlock on the abandoned callback")
    .expect("a fresh protocol session handles the next dispatch");
    assert_eq!(next["host_said"]["pong"], "after cancel");

    Arc::try_unwrap(host)
        .ok()
        .expect("host is sole owner")
        .into_inner()
        .shutdown()
        .await
        .unwrap();
}

#[tokio::test]
async fn plugin_cannot_read_host_env() {
    // The invariant D-22 enforces: a plugin process is launched env-cleared (the single guarded spawn
    // path), so it cannot read the host's secrets directly — it must request them through the gated
    // host capabilities. Set a non-allow-listed var in the host, spawn the plugin, and confirm the
    // plugin's own `std::env` can't see it.
    std::env::set_var("FLUX_TEST_PLUGIN_SECRET", "leak-me-not");
    let exe = env!("CARGO_BIN_EXE_caps_plugin");
    let system = test_system();
    let mut host = PluginHost::spawn(&system, exe, &[]).await.unwrap();
    std::env::remove_var("FLUX_TEST_PLUGIN_SECRET");

    let leaked = host
        .call("readenv", json!({ "key": "FLUX_TEST_PLUGIN_SECRET" }))
        .await
        .unwrap();
    assert_eq!(
        leaked["value"],
        Value::Null,
        "plugin inherited a host secret env var — the spawn path must clear the environment"
    );

    // Sanity anchor: an allow-listed var (PATH) DOES reach the plugin, proving the probe really reads
    // its own env (so the null above is isolation, not a broken probe).
    let allowed = host
        .call("readenv", json!({ "key": "PATH" }))
        .await
        .unwrap();
    assert!(
        allowed["value"].is_string(),
        "allow-listed PATH should pass through to the plugin"
    );

    host.shutdown().await.unwrap();
}

#[tokio::test]
async fn plugin_operations_project_as_tools() {
    use flux_plugin::{load_plugin_tools, DenyHostCaps};

    let exe = env!("CARGO_BIN_EXE_echo_plugin");
    let system = test_system();
    let desc = flux_plugin::PluginDescriptor {
        program: exe.to_string(),
        ..Default::default()
    };
    let flux_plugin::LoadedPlugin { tools, host, .. } =
        load_plugin_tools(&system, "echo", &desc, |_| Arc::new(DenyHostCaps))
            .await
            .unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].spec().name, "echo.upper");
    assert_eq!(
        tools[0].permission_subjects(&json!({})),
        vec!["echo.upper".to_string()]
    );
    // The op declares no effects, so it projects a conservative effect set and is NOT a no-op for
    // the authorization floor (which would otherwise skip plugin ops entirely).
    assert!(
        !tools[0].spec().effects.is_empty(),
        "plugin op must declare effects so the policy floor gates it"
    );

    // Drive the projected tool through the Tool interface.
    let dir = std::env::temp_dir().join(format!("flux-plugintool-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ctx = flux_runtime::ToolContext::new(Arc::new(flux_system::System::new(
        flux_system::Workspace::new(&dir).unwrap(),
    )));
    let r = tools[0].execute(&ctx, json!({"text": "hi"})).await.unwrap();
    assert!(!r.is_error);
    assert!(r.content.contains("HI"));

    // Release the tools' shared host references, then shut the subprocess down.
    drop(tools);
    Arc::try_unwrap(host)
        .ok()
        .expect("host is sole owner")
        .into_inner()
        .shutdown()
        .await
        .unwrap();
    std::fs::remove_dir_all(&dir).ok();
}

/// Monotonic suffix for target-adjacent writable copies; the two hash-drift tests may run in
/// parallel inside one integration-test process.
static NEXT_HASH_DRIFT_FIXTURE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[derive(Debug)]
struct HashDriftFixture {
    dir: std::path::PathBuf,
    stored: std::path::PathBuf,
}

impl Drop for HashDriftFixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

fn stage_hash_drift_fixture_with(
    exe: &std::path::Path,
    copy: impl FnOnce(&std::path::Path, &std::path::Path) -> std::io::Result<u64>,
) -> Result<HashDriftFixture, String> {
    use std::sync::atomic::Ordering;

    let target_dir = exe.parent().ok_or_else(|| {
        format!(
            "could not stage plugin fixture before hash verification: Cargo binary has no parent: \
             {}",
            exe.display()
        )
    })?;
    let sequence = NEXT_HASH_DRIFT_FIXTURE.fetch_add(1, Ordering::Relaxed);
    let dir = target_dir.join(format!(
        "flux-spawn-drift-{}-{sequence}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).map_err(|error| {
        format!(
            "could not stage plugin fixture in target-adjacent scratch directory `{}` before hash \
             verification: {error}",
            dir.display()
        )
    })?;

    let stored = dir.join("flux-plugin-echo");
    if let Err(error) = copy(exe, &stored) {
        std::fs::remove_dir_all(&dir).ok();
        return Err(format!(
            "could not stage plugin fixture in target-adjacent scratch directory `{}`: copying \
             `{}` to `{}` failed before hash verification: {error}",
            dir.display(),
            exe.display(),
            stored.display()
        ));
    }

    Ok(HashDriftFixture { dir, stored })
}

#[test]
fn hash_drift_fixture_copy_reports_storage_pressure_as_an_environment_failure() {
    let exe = std::path::Path::new(env!("CARGO_BIN_EXE_echo_plugin"));
    let simulated = std::io::Error::other("simulated ENOSPC / quota exceeded");
    let err = stage_hash_drift_fixture_with(exe, |_, _| Err(simulated))
        .expect_err("a fixture copy failure must be reported");

    let target_dir = exe.parent().expect("Cargo binary has a target directory");
    assert!(
        err.contains(&target_dir.display().to_string()),
        "the failure must name the target-adjacent scratch location: {err}"
    );
    assert!(
        err.contains("ENOSPC") && err.contains("quota exceeded"),
        "the failure must preserve the storage cause: {err}"
    );
    assert!(
        err.contains("before hash verification"),
        "the failure must identify fixture staging, not implicate hash verification: {err}"
    );
}

/// D-48 acceptance: a descriptor carrying a `sha256` is re-hashed before spawn — a tampered
/// binary is a hard refusal naming the plugin and both hashes; the untampered binary loads; a
/// hashless (dev/local) descriptor spawns exactly as before.
#[tokio::test]
async fn spawn_refuses_hash_drift() {
    let exe = std::path::Path::new(env!("CARGO_BIN_EXE_echo_plugin"));
    let fixture = stage_hash_drift_fixture_with(exe, |source, destination| {
        std::fs::copy(source, destination)
    })
    .unwrap_or_else(|error| panic!("{error}"));
    let stored = &fixture.stored;
    let recorded = flux_plugin::pack::sha256_hex(&std::fs::read(stored).unwrap());
    let system = test_system();

    // Untampered: the recorded hash matches → the plugin loads normally.
    let desc = flux_plugin::PluginDescriptor {
        program: stored.to_string_lossy().into_owned(),
        sha256: Some(recorded.clone()),
        ..Default::default()
    };
    let mut host = PluginHost::spawn_verified(&system, "echo", &desc)
        .await
        .expect("matching hash spawns");
    assert_eq!(host.manifest().await.unwrap().name, "echo");
    let _ = host.shutdown().await;

    // Tamper the stored binary → the same descriptor is a hard refusal naming plugin + hashes.
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(stored)
            .unwrap();
        f.write_all(b"tampered").unwrap();
    }
    let err = match PluginHost::spawn_verified(&system, "echo", &desc).await {
        Err(e) => e.to_string(),
        Ok(_) => panic!("hash drift must refuse to spawn"),
    };
    let actual = flux_plugin::pack::sha256_hex(&std::fs::read(stored).unwrap());
    assert!(err.contains("echo"), "names the plugin: {err}");
    assert!(err.contains(&recorded), "names the expected hash: {err}");
    assert!(err.contains(&actual), "names the actual hash: {err}");

    // Hashless (dev/local) descriptors spawn as today — even over the tampered file.
    let dev = flux_plugin::PluginDescriptor {
        program: stored.to_string_lossy().into_owned(),
        ..Default::default()
    };
    let host = PluginHost::spawn_verified(&system, "echo", &dev)
        .await
        .expect("hashless descriptor spawns unverified");
    let _ = host.shutdown().await;
}

/// C-90 end-to-end: an op's per-operation `process` narrowing governs both what the approval
/// layer is told (`process.exec` names the narrowed argv prefix) and what the callback gate
/// actually admits — an argv inside the manifest-wide grant but outside the op's declaration is
/// refused at the host boundary.
#[tokio::test]
async fn op_process_narrowing_gates_callbacks_and_names_authority() {
    use flux_plugin::{load_plugin_tools, SystemHostCaps};

    let exe = env!("CARGO_BIN_EXE_caps_plugin");
    let system = test_system();
    let desc = flux_plugin::PluginDescriptor {
        program: exe.to_string(),
        ..Default::default()
    };
    let sys = Arc::new(test_system());
    let flux_plugin::LoadedPlugin { tools, host, .. } =
        load_plugin_tools(&system, "caps", &desc, |manifest| {
            Arc::new(SystemHostCaps::new(sys.clone()).with_grants(manifest.capabilities.clone()))
        })
        .await
        .unwrap();
    let runproc = tools
        .iter()
        .find(|t| t.spec().name == "caps.runproc")
        .expect("runproc projects as a tool");

    // The narrowed prefix IS the disclosed authority — not the manifest-wide `printf`.
    let requirements = runproc
        .authority_requirements(&json!({}), &["caps.runproc".to_string()])
        .unwrap();
    let process_resources: Vec<&str> = requirements
        .iter()
        .filter(|r| r.action.0 == "process.exec")
        .map(|r| r.resource.id.as_str())
        .collect();
    assert_eq!(
        process_resources,
        vec!["printf ok"],
        "authority must name the op's narrowed argv prefix"
    );

    let dir = std::env::temp_dir().join(format!("flux-opnarrow-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ctx = flux_runtime::ToolContext::new(Arc::new(flux_system::System::new(
        flux_system::Workspace::new(&dir).unwrap(),
    )));

    // Inside the op narrowing: runs.
    let ok = runproc
        .execute(&ctx, json!({"argv": ["printf", "ok"]}))
        .await
        .unwrap();
    assert!(!ok.is_error, "{}", ok.content);
    assert!(ok.content.contains("\"exit_code\": 0"), "{}", ok.content);

    // Inside the manifest grant (`printf …`) but outside the op's declaration: refused, naming
    // the operation.
    let denied = runproc
        .execute(&ctx, json!({"argv": ["printf", "nope"]}))
        .await
        .unwrap();
    assert!(denied.is_error, "{}", denied.content);
    assert!(
        denied.content.contains("declared process constraints"),
        "{}",
        denied.content
    );

    drop(tools);
    Arc::try_unwrap(host)
        .ok()
        .expect("host is sole owner")
        .into_inner()
        .shutdown()
        .await
        .unwrap();
    std::fs::remove_dir_all(&dir).ok();
}

/// A plugin speaking a different wire protocol must be rejected at the seam, naming both markers.
///
/// The version lockstep between flux and the plugin pack was removed (C-143), so this check is what
/// enforces compatibility now. Without it an incompatible plugin surfaces as an opaque
/// deserialization failure with nothing pointing at the real cause.
#[tokio::test]
async fn a_plugin_speaking_a_foreign_protocol_is_rejected_with_both_markers_named() {
    let exe = env!("CARGO_BIN_EXE_future_protocol_plugin");
    let system = test_system();
    let mut host = PluginHost::spawn(&system, exe, &[]).await.unwrap();

    let err = host
        .manifest()
        .await
        .expect_err("a foreign protocol marker must not be accepted");
    let message = err.to_string();
    assert!(
        message.contains("flux.plugin.v99"),
        "names the plugin's marker: {message}"
    );
    assert!(
        message.contains(flux_plugin::PROTOCOL),
        "names the host's marker: {message}"
    );
}
