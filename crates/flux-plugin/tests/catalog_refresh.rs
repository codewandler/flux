//! C-310 — catalog refresh: re-project a loaded plugin's operations from a **second** `manifest`
//! fetch on the already-open subprocess, without restarting flux.
//!
//! Every test here drives `drift_plugin`, the one fixture whose `manifest` response changes between
//! calls (it re-reads a mode file passed as `argv[1]`). The load-time manifest is always `base`
//! (`alpha` + `beta`); the test rewrites the mode file and refreshes.
//!
//! The safety half is the point of most of these: a refresh is a *re-grant*, so a plugin must not be
//! able to answer `manifest` differently the second time and come away with more authority than the
//! operator granted at load.

use std::sync::Arc;

use flux_plugin::{load_plugin_tools, LoadedPlugin, PluginDescriptor, SystemHostCaps};
use flux_runtime::ToolRegistry;
use serde_json::json;

/// A throwaway workspace-rooted `System` — the guarded spawn path needs one; these tests do no
/// file IO of their own through it.
fn test_system() -> flux_system::System {
    flux_system::System::new(flux_system::Workspace::new(std::env::temp_dir()).unwrap())
}

/// A temp dir that removes itself on drop, so a failing assertion cannot leak it.
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "flux-c310-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("create temp dir");
        TempDir(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The load-time state: the `drift` plugin loaded from its `base` manifest, its tools registered,
/// and the mode file the test rewrites to change what the next `manifest` call answers.
struct Fixture {
    _dir: TempDir,
    mode_file: std::path::PathBuf,
    loaded: LoadedPlugin,
    registry: ToolRegistry,
}

impl Fixture {
    async fn load(tag: &str) -> Self {
        let dir = TempDir::new(tag);
        let mode_file = dir.0.join("mode");
        std::fs::write(&mode_file, "base").unwrap();
        let descriptor = PluginDescriptor {
            program: env!("CARGO_BIN_EXE_drift_plugin").to_string(),
            args: vec![mode_file.to_string_lossy().into_owned()],
            ..Default::default()
        };
        let system = test_system();
        let caps_system = Arc::new(test_system());
        let loaded = load_plugin_tools(&system, "drift", &descriptor, move |manifest| {
            Arc::new(SystemHostCaps::new(caps_system).with_grants(manifest.capabilities.clone()))
        })
        .await
        .expect("the base manifest loads");

        let mut registry = ToolRegistry::new();
        for tool in &loaded.tools {
            registry
                .try_register_from("plugin:drift", tool.clone())
                .expect("base catalog registers");
        }
        assert_eq!(
            registry.names(),
            vec!["drift.alpha".to_string(), "drift.beta".to_string()],
            "load-time catalog"
        );
        Self {
            _dir: dir,
            mode_file,
            loaded,
            registry,
        }
    }

    fn set_mode(&self, mode: &str) {
        std::fs::write(&self.mode_file, mode).unwrap();
    }

    /// Refresh and apply in one step — the shape a caller uses when it holds the registry.
    async fn refresh_into_registry(&mut self) {
        let refresh = self.loaded.refresh().await.expect("refresh is accepted");
        refresh
            .apply(&mut self.registry, "plugin:drift")
            .expect("the refreshed catalog applies");
    }

    async fn shutdown(self) {
        let Fixture {
            loaded, registry, ..
        } = self;
        let LoadedPlugin { tools, host, .. } = loaded;
        drop(tools);
        drop(registry);
        if let Ok(host) = Arc::try_unwrap(host) {
            let _ = host.into_inner().shutdown().await;
        }
    }
}

/// **The failing-first test.** A plugin whose `manifest` answers differently on the second call has
/// its new op projected into the live registry and its withdrawn op removed — no restart, no
/// respawn. This is the behavior the connectors seam is built on.
#[tokio::test]
async fn refresh_reprojects_a_changed_catalog_into_the_registry() {
    let mut fixture = Fixture::load("reproject").await;

    // The operator authenticates a provider inside the deployment the plugin fronts; the plugin's
    // next `manifest` advertises `gamma` and withdraws `beta`.
    fixture.set_mode("grown");
    let refresh = fixture.loaded.refresh().await.expect("refresh is accepted");

    assert_eq!(refresh.added, vec!["drift.gamma".to_string()]);
    assert_eq!(refresh.removed, vec!["drift.beta".to_string()]);
    assert_eq!(refresh.retained, vec!["drift.alpha".to_string()]);

    refresh
        .apply(&mut fixture.registry, "plugin:drift")
        .expect("the refreshed catalog applies");
    assert_eq!(
        fixture.registry.names(),
        vec!["drift.alpha".to_string(), "drift.gamma".to_string()],
        "the new op is callable and the withdrawn one is gone"
    );
    // The `LoadedPlugin` itself now carries the refreshed catalog, so a later refresh diffs
    // against this one rather than against the load.
    assert_eq!(
        fixture
            .loaded
            .tools
            .iter()
            .map(|t| t.spec().name)
            .collect::<Vec<_>>(),
        vec!["drift.alpha".to_string(), "drift.gamma".to_string()],
    );
    fixture.shutdown().await;
}

/// A withdrawn op is *withdrawn*, not shadowed: it is gone from the registry, and re-registering the
/// name is free. A tool handle already taken out of the registry — an in-flight call — keeps running
/// against the spec it was projected with; withdrawal governs future dispatch only.
#[tokio::test]
async fn a_withdrawn_op_is_removed_while_an_in_flight_call_completes_under_its_old_spec() {
    let mut fixture = Fixture::load("withdrawn").await;

    // Take the handle the way a dispatch in progress holds one.
    let in_flight = fixture
        .registry
        .get("drift.beta")
        .expect("beta is registered at load");
    let spec_before = in_flight.spec();

    fixture.set_mode("grown");
    fixture.refresh_into_registry().await;

    assert!(
        fixture.registry.get("drift.beta").is_none(),
        "a withdrawn op must not be dispatchable"
    );
    // Not merely shadowed by a later entry: nothing named `drift.beta` is in the registry at all,
    // so registering the name again succeeds rather than colliding.
    assert!(
        !fixture.registry.names().contains(&"drift.beta".to_string()),
        "withdrawn, not shadowed"
    );

    // The in-flight handle still describes exactly what it was authorized as — a refresh cannot
    // retroactively re-scope a call that is already running.
    assert_eq!(in_flight.spec().risk, spec_before.risk);
    assert_eq!(in_flight.spec().effects, spec_before.effects);
    assert_eq!(in_flight.spec().access, spec_before.access);

    // And it completes rather than panicking or hanging: the subprocess is still open (the other
    // ops share it), the plugin no longer serves `beta`, so the call comes back as a tool error.
    let ctx = flux_runtime::ToolContext::new(Arc::new(test_system()));
    let result = in_flight
        .execute(&ctx, json!({"text": "hi"}))
        .await
        .expect("the call completes");
    assert!(
        result.is_error && result.content.contains("unknown operation"),
        "an in-flight call to a withdrawn op resolves as a plugin error: {}",
        result.content
    );

    drop(in_flight);
    fixture.shutdown().await;
}

/// The escalation guard. A refresh may change the *op set* freely, but it may not widen the
/// capability families the operator granted at load — programs, secret keys, HTTP hosts, or dial
/// targets. Each is refused by name, and the previously registered ops survive untouched.
#[tokio::test]
async fn a_refresh_cannot_widen_the_granted_capabilities() {
    for (mode, family, widened) in [
        ("widen-process", "process", "printf"),
        ("widen-secrets", "secrets", "EXTRA_KEY"),
        ("widen-http", "http_hosts", "attacker.example.com"),
        ("widen-conn", "conn", "tcp:*:5432"),
    ] {
        let mut fixture = Fixture::load(&format!("widen-{family}")).await;
        fixture.set_mode(mode);

        let error = fixture
            .loaded
            .refresh()
            .await
            .expect_err("a widening refresh must be refused")
            .to_string();
        assert!(
            error.contains(family) && error.contains(widened),
            "the refusal names the capability family and the entry it tried to add: {error}"
        );

        // Refused, and nothing moved: the registry, the LoadedPlugin's tools, and the caps the
        // subprocess actually runs under are all the load-time ones.
        assert_eq!(
            fixture.registry.names(),
            vec!["drift.alpha".to_string(), "drift.beta".to_string()],
            "{mode}: the registered ops survive a refused refresh"
        );
        assert_eq!(
            fixture.loaded.manifest.capabilities.process,
            vec!["printf ok".to_string()],
            "{mode}: the granted capabilities are unchanged"
        );
        fixture.shutdown().await;
    }
}

/// The other half of the escalation guard: an op that keeps its **name** may not quietly become a
/// differently-scoped op. A session grant / policy rule keys on `drift.alpha`, so a refresh that
/// drops its risk tier and its per-operation `process` narrowing is a re-scope under a stable name.
#[tokio::test]
async fn a_refresh_cannot_weaken_a_retained_ops_gating_scope() {
    let mut fixture = Fixture::load("weaken").await;
    fixture.set_mode("weaken-op");

    let error = fixture
        .loaded
        .refresh()
        .await
        .expect_err("a scope-weakening refresh must be refused")
        .to_string();
    assert!(
        error.contains("drift.alpha"),
        "the refusal names the op: {error}"
    );
    assert!(
        error.contains("risk") && error.contains("process"),
        "the refusal names both weakenings: {error}"
    );

    assert_eq!(
        fixture.registry.names(),
        vec!["drift.alpha".to_string(), "drift.beta".to_string()],
    );
    // The still-registered `alpha` keeps the tier it was granted under.
    assert_eq!(
        fixture.registry.get("drift.alpha").unwrap().spec().risk,
        flux_spec::Risk::High,
    );
    fixture.shutdown().await;
}

/// A refresh re-runs the load-time manifest validation. A manifest that would have been refused at
/// load is refused at refresh, and the catalog is untouched.
#[tokio::test]
async fn a_refresh_re_runs_manifest_validation() {
    let mut fixture = Fixture::load("invalid").await;
    fixture.set_mode("invalid");

    let error = fixture
        .loaded
        .refresh()
        .await
        .expect_err("an invalid manifest must be refused")
        .to_string();
    assert!(
        error.contains("duplicate operation"),
        "the refusal is the same one `validate_manifest_operations` gives at load: {error}"
    );
    assert_eq!(
        fixture.registry.names(),
        vec!["drift.alpha".to_string(), "drift.beta".to_string()],
    );
    fixture.shutdown().await;
}

/// C-191 coherence warnings are computed for the refreshed manifest exactly as they are at load —
/// and, exactly as at load, they *warn* rather than refuse: removing the op would cost the
/// capability without buying safety.
#[tokio::test]
async fn a_refresh_reports_coherence_warnings_without_refusing_the_catalog() {
    let mut fixture = Fixture::load("coherence").await;
    assert!(
        fixture.loaded.coherence_warnings.is_empty(),
        "the base manifest is coherent"
    );

    fixture.set_mode("incoherent");
    let refresh = fixture
        .loaded
        .refresh()
        .await
        .expect("warnings do not refuse");
    assert!(
        refresh
            .coherence_warnings
            .iter()
            .any(|w| w.contains("drift.delta") && w.contains("I2")),
        "the refreshed manifest's incoherent op is named: {:?}",
        refresh.coherence_warnings
    );

    refresh
        .apply(&mut fixture.registry, "plugin:drift")
        .expect("apply");
    assert!(
        fixture
            .registry
            .names()
            .contains(&"drift.delta".to_string()),
        "an under-declared op still loads, as it does at load time"
    );
    // The warnings are carried on the plugin too, so a later reader sees the refreshed set.
    assert_eq!(
        fixture.loaded.coherence_warnings,
        refresh.coherence_warnings
    );
    fixture.shutdown().await;
}

/// A transport failure mid-refresh never half-applies. The subprocess dies while answering the
/// second `manifest`; the refresh reports it and every previously registered op is still there.
/// The oversized-frame and protocol-decode failures reach the same place — an `Err` out of
/// `PluginHost::manifest` before anything is swapped.
#[tokio::test]
async fn a_refresh_against_a_dead_subprocess_leaves_the_catalog_intact() {
    let mut fixture = Fixture::load("dead").await;
    fixture.set_mode("die");

    let error = fixture
        .loaded
        .refresh()
        .await
        .expect_err("a dead subprocess must fail the refresh")
        .to_string();
    assert!(
        error.contains("closed the connection"),
        "the failure is reported, not swallowed: {error}"
    );
    assert_eq!(
        fixture.registry.names(),
        vec!["drift.alpha".to_string(), "drift.beta".to_string()],
    );
    assert_eq!(fixture.loaded.tools.len(), 2);
    fixture.shutdown().await;
}

/// An oversized manifest frame is refused by the host's frame bound, and the catalog survives it —
/// the same all-or-nothing path as a dead subprocess.
#[tokio::test]
async fn a_refresh_with_an_oversized_manifest_frame_leaves_the_catalog_intact() {
    let mut fixture = Fixture::load("oversized").await;
    fixture.set_mode("oversized");

    let error = fixture
        .loaded
        .refresh()
        .await
        .expect_err("an oversized frame must fail the refresh")
        .to_string();
    assert!(
        !error.is_empty(),
        "the frame-bound failure is reported: {error}"
    );
    assert_eq!(
        fixture.registry.names(),
        vec!["drift.alpha".to_string(), "drift.beta".to_string()],
    );
    fixture.shutdown().await;
}
