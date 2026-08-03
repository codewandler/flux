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
use flux_runtime::{LiveToolCatalog, ToolRegistry};
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
    /// The whole load-time manifest, kept so a test can assert what a refresh must not move.
    granted: flux_plugin::PluginManifest,
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
            granted: loaded.manifest.clone(),
            loaded,
            registry,
        }
    }

    fn set_mode(&self, mode: &str) {
        std::fs::write(&self.mode_file, mode).unwrap();
    }

    /// Refresh and install into the registry in one step — the entry point production callers use.
    async fn refresh_into_registry(&mut self) {
        self.loaded
            .refresh_into(&mut self.registry, "plugin:drift")
            .await
            .expect("refresh is accepted");
    }

    /// The invariant every accepted refresh must preserve: the authority *declaration* the plugin
    /// carries is still the operator's load-time grant, so the specs the registry installs and the
    /// capabilities the pinned host caps enforce are computed from one value and cannot disagree.
    fn assert_grant_is_pinned(&self) {
        // Compared as JSON: none of these wire structs derive `PartialEq`, and they all serialize.
        assert_eq!(
            serde_json::to_value(&self.loaded.manifest.capabilities).unwrap(),
            serde_json::to_value(&self.granted.capabilities).unwrap(),
            "a refresh must never move the granted capabilities"
        );
        assert_eq!(
            serde_json::to_value(&self.loaded.manifest.endpoints).unwrap(),
            serde_json::to_value(&self.granted.endpoints).unwrap(),
            "a refresh must never move the declared endpoints (a second egress surface)"
        );
        assert_eq!(
            serde_json::to_value(&self.loaded.manifest.auth).unwrap(),
            serde_json::to_value(&self.granted.auth).unwrap(),
            "a refresh must never move the declared auth purposes"
        );
        assert_eq!(
            serde_json::to_value(&self.loaded.manifest.config).unwrap(),
            serde_json::to_value(&self.granted.config).unwrap(),
            "a refresh must never move the declared config surface"
        );
        assert_eq!(
            serde_json::to_value(&self.loaded.manifest.discovers).unwrap(),
            serde_json::to_value(&self.granted.discovers).unwrap(),
            "a refresh must never move the discoverable product set (C-322)"
        );
    }

    /// The full authority footprint of one registered op — what the authorization floor reads.
    fn authority_of(&self, name: &str) -> (Vec<flux_spec::AccessKind>, Vec<String>) {
        let tool = self
            .registry
            .get(name)
            .unwrap_or_else(|| panic!("`{name}` is registered"));
        let subjects = tool.permission_subjects(&json!({}));
        let mut requirements: Vec<String> = tool
            .authority_requirements(&json!({}), &subjects)
            .expect("authority contract is valid")
            .iter()
            .map(|r| format!("{}:{}", r.action.0, r.resource.id))
            .collect();
        requirements.sort();
        (tool.spec().access, requirements)
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
    // What a refresh must never move, even on the happy path.
    fixture.assert_grant_is_pinned();
    fixture.shutdown().await;
}

/// C-318: the plugin host can publish the same atomic refresh to a long-lived session without
/// taking `&mut` access to its executor. The published generation is complete, while a snapshot
/// already held by an active turn remains unchanged.
#[tokio::test]
async fn refresh_live_publishes_for_the_next_turn_without_rewriting_an_adopted_snapshot() {
    let mut fixture = Fixture::load("live-session").await;
    let catalog = LiveToolCatalog::new(fixture.registry.clone());
    let active_turn = catalog.snapshot();

    fixture.set_mode("grown");
    fixture
        .loaded
        .refresh_live(&catalog, "plugin:drift")
        .await
        .expect("the live refresh is accepted");

    assert_eq!(
        active_turn.names(),
        vec!["drift.alpha".to_string(), "drift.beta".to_string()],
        "an already-adopted turn generation must not move"
    );
    assert_eq!(
        catalog.snapshot().names(),
        vec!["drift.alpha".to_string(), "drift.gamma".to_string()],
        "the next turn sees the gain and withdrawal together"
    );
    assert_eq!(
        fixture
            .loaded
            .tools
            .iter()
            .map(|tool| tool.spec().name)
            .collect::<Vec<_>>(),
        vec!["drift.alpha".to_string(), "drift.gamma".to_string()],
        "the plugin commit and published generation stay aligned"
    );

    fixture.shutdown().await;
}

/// A failed registry write must not leave the plugin believing it published a catalog the registry
/// never took. `refresh_into` writes the registry first precisely so this cannot happen: if the two
/// diverged, the next refresh would diff against the newer manifest and the stale names could never
/// be withdrawn.
#[tokio::test]
async fn a_refused_registry_write_keeps_the_plugin_and_the_registry_in_step() {
    let mut fixture = Fixture::load("divergence").await;

    // Another source already owns the name the refreshed catalog is about to claim, so `apply`
    // fails on a duplicate.
    let squatter = fixture
        .registry
        .get("drift.alpha")
        .expect("alpha is registered at load");
    let mut colliding = ToolRegistry::new();
    colliding
        .try_register_from("some-other-pack", squatter)
        .unwrap();

    fixture.set_mode("grown");
    let error = fixture
        .loaded
        .refresh_into(&mut colliding, "plugin:drift")
        .await
        .expect_err("the colliding registry write must fail")
        .to_string();
    assert!(error.contains("duplicate operation"), "{error}");

    // Neither side moved: the foreign registry still holds only its own entry, still owned by the
    // pack that registered it — a refresh withdraws only what its own source put there, so it can
    // never evict another pack's identically named op.
    assert_eq!(colliding.names(), vec!["drift.alpha".to_string()]);
    assert_eq!(colliding.source("drift.alpha"), Some("some-other-pack"));
    assert_eq!(
        fixture
            .loaded
            .tools
            .iter()
            .map(|t| t.spec().name)
            .collect::<Vec<_>>(),
        vec!["drift.alpha".to_string(), "drift.beta".to_string()],
        "a rejected apply must not commit the plugin's catalog"
    );

    // And because it did not move, a later refresh against the real registry still diffs from the
    // load — so `drift.beta` is still withdrawable rather than stranded.
    fixture.refresh_into_registry().await;
    assert_eq!(
        fixture.registry.names(),
        vec!["drift.alpha".to_string(), "drift.gamma".to_string()],
    );
    drop(colliding);
    fixture.shutdown().await;
}

/// The running-session publication path has the same all-or-nothing collision behavior as the
/// mutable-registry path: neither the published generation nor LoadedPlugin commits on failure.
#[tokio::test]
async fn a_refused_live_publication_keeps_plugin_and_catalog_on_the_old_generation() {
    let mut fixture = Fixture::load("live-divergence").await;
    let squatter = fixture.registry.get("drift.alpha").unwrap();
    let mut colliding = ToolRegistry::new();
    colliding
        .try_register_from("some-other-pack", squatter)
        .unwrap();
    let catalog = LiveToolCatalog::new(colliding);

    fixture.set_mode("grown");
    let error = fixture
        .loaded
        .refresh_live(&catalog, "plugin:drift")
        .await
        .expect_err("the live collision must reject the whole generation")
        .to_string();
    assert!(error.contains("duplicate operation"), "{error}");
    assert_eq!(catalog.snapshot().names(), vec!["drift.alpha".to_string()]);
    assert_eq!(
        fixture
            .loaded
            .tools
            .iter()
            .map(|tool| tool.spec().name)
            .collect::<Vec<_>>(),
        vec!["drift.alpha".to_string(), "drift.beta".to_string()],
        "LoadedPlugin must not commit a generation the live catalog rejected"
    );

    drop(catalog);
    fixture.shutdown().await;
}

/// A withdrawn op is *withdrawn*, not shadowed. A call that actually entered the old subprocess
/// handler before refresh completes successfully; refresh waits for that host exchange, then
/// publishes the withdrawal for future dispatch.
#[tokio::test]
async fn a_withdrawn_op_is_removed_while_an_in_flight_call_completes_under_its_old_spec() {
    let mut fixture = Fixture::load("withdrawn").await;
    let in_flight = fixture
        .registry
        .get("drift.beta")
        .expect("beta is registered at load");
    let spec_before = in_flight.spec();
    let entered = fixture._dir.0.join("entered");
    let release = fixture._dir.0.join("release");
    let ctx = flux_runtime::ToolContext::new(Arc::new(test_system()));
    let entered_arg = entered.to_string_lossy().into_owned();
    let release_arg = release.to_string_lossy().into_owned();
    let call = tokio::spawn({
        let tool = in_flight.clone();
        async move {
            tool.execute(
                &ctx,
                json!({
                    "text": "hi",
                    "entered_path": entered_arg,
                    "release_path": release_arg,
                }),
            )
            .await
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while !entered.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the old beta call enters the subprocess handler");

    let catalog = LiveToolCatalog::new(fixture.registry.clone());
    fixture.set_mode("grown");
    let result = {
        let refresh = fixture.loaded.refresh_live(&catalog, "plugin:drift");
        tokio::pin!(refresh);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), refresh.as_mut())
                .await
                .is_err(),
            "refresh must not tear down or replace a host exchange already in flight"
        );
        std::fs::write(&release, "release").unwrap();

        let result = call
            .await
            .expect("call task joins")
            .expect("the already-running old call completes");
        refresh.await.expect("refresh completes after the old call");
        result
    };

    assert!(
        catalog.snapshot().get("drift.beta").is_none(),
        "a withdrawn op must not be dispatchable"
    );
    // Not merely shadowed by a later entry: nothing named `drift.beta` is in the registry at all,
    // so registering the name again succeeds rather than colliding.
    assert!(
        !catalog
            .snapshot()
            .names()
            .contains(&"drift.beta".to_string()),
        "withdrawn, not shadowed"
    );

    // The in-flight handle still describes exactly what it was authorized as — a refresh cannot
    // retroactively re-scope a call that is already running.
    assert_eq!(in_flight.spec().risk, spec_before.risk);
    assert_eq!(in_flight.spec().effects, spec_before.effects);
    assert_eq!(in_flight.spec().access, spec_before.access);

    let payload: serde_json::Value = serde_json::from_str(&result.content).unwrap();
    assert!(!result.is_error, "old call failed: {}", result.content);
    assert_eq!(
        payload.get("operation").and_then(serde_json::Value::as_str),
        Some("beta"),
        "the call admitted under the old catalog must succeed"
    );

    drop(in_flight);
    drop(catalog);
    fixture.shutdown().await;
}

/// The **surrender** direction, and the more dangerous one. A refreshed manifest that gives up
/// capabilities must not thereby strip its ops' declared authority, because the host capabilities
/// are pinned and still grant the secret / host / program. If `access` and the
/// `AuthorityRequirement`s were derived from the surrendered declaration, the op would sail past
/// the authorization floor requiring nothing at all while every capability stayed live — the exact
/// shape `plugin_tool_spec` warns about ("would carry NO requirement at all and skip the
/// authorization floor entirely").
#[tokio::test]
async fn a_surrendered_capability_declaration_cannot_strip_an_ops_authority() {
    let mut fixture = Fixture::load("surrender").await;

    // The authority `drift.alpha` was granted and gated with at load.
    let (access_at_load, requirements_at_load) = fixture.authority_of("drift.alpha");
    assert!(
        access_at_load.contains(&flux_spec::AccessKind::Secret)
            && access_at_load.contains(&flux_spec::AccessKind::Network)
            && access_at_load.contains(&flux_spec::AccessKind::Connection),
        "the load-time op must actually hold the authority under test: {access_at_load:?}"
    );
    assert!(
        !requirements_at_load.is_empty(),
        "the load-time op must require authority"
    );

    // The plugin now answers `manifest` with the same operations and an emptied capability set.
    fixture.set_mode("surrender");
    fixture.refresh_into_registry().await;

    let (access_after, requirements_after) = fixture.authority_of("drift.alpha");
    assert_eq!(
        access_after, access_at_load,
        "a surrender must not strip the op's declared access — the pinned host caps still grant it"
    );
    assert_eq!(
        requirements_after, requirements_at_load,
        "a surrender must not strip the op's authority requirements"
    );
    // The root cause, asserted directly: one value feeds both the projection and the enforcement.
    fixture.assert_grant_is_pinned();
    fixture.shutdown().await;
}

/// The other authority-bearing manifest fields `SystemHostCaps::with_manifest` reads — `endpoints`,
/// `auth`, `config` — are pinned for the same reason. A plugin's declared endpoint hosts are
/// admitted as egress alongside `http_hosts`, so letting them travel across a refresh would let the
/// stored manifest advertise reach the pinned caps do not back.
#[tokio::test]
async fn a_refresh_cannot_move_the_other_pinned_authority_fields() {
    let mut fixture = Fixture::load("endpoints").await;
    assert_eq!(
        fixture.granted.endpoints.len(),
        1,
        "the load-time manifest must declare the endpoint under test"
    );

    fixture.set_mode("drift-endpoints");
    fixture.refresh_into_registry().await;

    fixture.assert_grant_is_pinned();
    assert!(
        !format!("{:?}", fixture.loaded.manifest.endpoints).contains("attacker.example.com"),
        "the refreshed endpoint declaration must not be adopted: {:?}",
        fixture.loaded.manifest.endpoints
    );
    fixture.shutdown().await;
}

/// C-322: `discovers` is pinned too. It is the *provider* side of endpoint discovery (D-26):
/// `PluginRegistry::providers_for` routes a consumer's query for product X to every plugin whose
/// manifest `discovers` X, and the broker commits what that provider answers into the shared
/// `EndpointRegistry` other components then resolve against. Enlisting for a product across a
/// refresh is therefore a plugin granting itself the authority to say where `postgres` lives —
/// authority the operator reviewed at approval (`plugin list` discloses `discovers` in the surface
/// line) and never gave for the new product.
///
/// It is inert *today* only by accident of wiring: `ProviderEntry` snapshots the manifest in an
/// `Arc` at load and refresh never re-registers it, so the broker still routes on the load-time
/// value. C-318 wires refresh into a live session and removes that accident. Pinning now means the
/// escalation cannot appear when it does.
#[tokio::test]
async fn a_refresh_cannot_move_the_discoverable_product_set() {
    let mut fixture = Fixture::load("discovers").await;
    assert_eq!(
        fixture.granted.discovers,
        vec!["prometheus".to_string()],
        "the load-time manifest must enlist for exactly the product under test"
    );

    fixture.set_mode("drift-discovers");
    fixture.refresh_into_registry().await;

    fixture.assert_grant_is_pinned();
    assert!(
        !fixture
            .loaded
            .manifest
            .discovers
            .iter()
            .any(|p| p == "postgres"),
        "a refresh must not enlist the plugin as a discovery provider for a product the operator \
         never approved it for: {:?}",
        fixture.loaded.manifest.discovers
    );
    fixture.shutdown().await;
}

/// **The exhaustiveness anchor** (C-322), the twin of the one `capability_widenings` carries for
/// `PluginCapabilities`. Adding a field to [`flux_plugin::PluginManifest`] reds *here* and in
/// `LoadedPlugin::pin_granted_authority`, which is the prompt to classify it — the `..fetched`
/// struct-update this replaced would otherwise adopt an authority-bearing field from the plugin's
/// *second* answer in total silence, which is C-310's round-1 surrender bug on a new surface.
///
/// The classification of record lives on `pin_granted_authority`; it is restated here so the two
/// cannot drift apart without one of them failing to compile.
#[test]
fn every_manifest_field_is_classified_pinned_or_adopted() {
    let m = flux_plugin::PluginManifest::default();
    let flux_plugin::PluginManifest {
        // PINNED — the operator's load-time grant, re-stated by `pin_granted_authority`.
        capabilities: _, // read by `SystemHostCaps::with_manifest` as the grant itself
        auth: _,         // read by `with_manifest`; resolves secrets by purpose
        endpoints: _,    // read by `with_manifest`; a second egress surface beside `http_hosts`
        config: _,       // read by `with_manifest`; the gated `config` capability's surface
        discovers: _,    // routes the D-26 discovery fan-out — see the test above
        // ADOPTED — the point of a refresh, or descriptive only.
        name: _,        // cannot change at all: refused before pinning runs (`refresh.rs`)
        operations: _, // *the* thing a refresh changes; re-validated against the PINNED capabilities
        version: _,    // descriptive
        groups: _,     // tool organisation, consumed once at load; no authority
        datasources: _, // display-only at its consumers; no authority
    } = &m;
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
