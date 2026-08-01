//! C-411 — a plugin's capability **widening** is not adopted at the next load.
//!
//! The in-session rules (C-310/C-311/C-312) all bound a *refresh* to the manifest the operator's
//! session started with. Nothing bounded the manifest a *new process* starts from: a plugin that
//! widened what it asks for between two loads had the wider set installed verbatim, and the grant
//! the operator reasoned about at install time was quietly no longer the grant in force.
//!
//! Every test here drives `platform_plugin` through its mode file (`argv[1]`), rewriting it between
//! two independent loads — the cross-process shape a plugin upgrade actually has.
//!
//! Two shapes are exercised deliberately, because they are two ingest paths and not one:
//! [`Store::load`] is the projected-tool path (`load_plugin_tools`, agent startup and the SDK), and
//! [`Store::call_path_manifest`] is the shape `flux plugin call` has — `spawn_verified`, a manifest
//! fetch of its own, and `SystemHostCaps::with_manifest` — which never touches `load_plugin_tools`
//! at all, and which `flux-codegate`'s C-404 census already lists as a distinct ingest site.

use std::sync::Arc;

use flux_plugin::{
    add_descriptor, load_descriptor, load_plugin_tools, PluginDescriptor, PluginHost,
    SystemHostCaps,
};

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
            "flux-c411-{tag}-{}-{:?}",
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

/// A plugin **store** (`~/.flux/plugins`-shaped) holding one descriptor for `platform`, plus the
/// mode file that decides what the plugin's next `manifest` answer declares.
struct Store {
    dir: TempDir,
    mode_file: std::path::PathBuf,
}

impl Store {
    /// Register `platform` in a fresh store, with its first manifest answer set to `mode`.
    fn new(tag: &str, mode: &str) -> Self {
        let dir = TempDir::new(tag);
        let mode_file = dir.0.join("mode");
        std::fs::write(&mode_file, mode).unwrap();
        add_descriptor(
            &dir.0,
            "platform",
            &PluginDescriptor {
                program: env!("CARGO_BIN_EXE_platform_plugin").to_string(),
                args: vec![mode_file.to_string_lossy().into_owned()],
                ..Default::default()
            },
        )
        .expect("register the plugin");
        Self { dir, mode_file }
    }

    fn set_mode(&self, mode: &str) {
        std::fs::write(&self.mode_file, mode).unwrap();
    }

    fn descriptor_text(&self) -> String {
        std::fs::read_to_string(self.dir.0.join("platform.toml")).expect("read the descriptor")
    }

    /// One whole load, exactly as a new flux process performs it: read the persisted descriptor,
    /// then spawn and project from it. Answers the projected tool names — `LoadedPlugin` is not
    /// `Debug`, and nothing here needs the handles.
    async fn load(&self) -> Result<Vec<String>, String> {
        let descriptor = load_descriptor(&self.dir.0, "platform")
            .expect("read the descriptor")
            .expect("the descriptor exists");
        let system = test_system();
        let caps_system = Arc::new(test_system());
        load_plugin_tools(&system, "platform", &descriptor, move |manifest| {
            Arc::new(SystemHostCaps::new(caps_system).with_grants(manifest.capabilities.clone()))
        })
        .await
        .map(|loaded| loaded.tools.iter().map(|t| t.spec().name.clone()).collect())
        .map_err(|e| e.to_string())
    }

    /// The `flux plugin call` shape: spawn from the descriptor, fetch the manifest, and report what
    /// authority `with_manifest` would install from it. No `load_plugin_tools` anywhere — this is
    /// the path that made "every load funnels through the projection helper" false.
    async fn call_path_manifest(&self) -> Result<Vec<String>, String> {
        let descriptor = load_descriptor(&self.dir.0, "platform")
            .expect("read the descriptor")
            .expect("the descriptor exists");
        let system = test_system();
        let mut host = PluginHost::spawn_verified(&system, "platform", &descriptor)
            .await
            .map_err(|e| e.to_string())?;
        let fetched = host.manifest().await.map_err(|e| e.to_string());
        let _ = host.shutdown().await;
        let manifest = fetched?;
        // Exactly what the CLI does with the answer: hand it to the capability layer verbatim.
        let caps = SystemHostCaps::new(Arc::new(test_system())).with_manifest(&manifest);
        let _ = caps;
        Ok(manifest
            .capabilities
            .http_hosts
            .iter()
            .cloned()
            .chain(manifest.endpoints.iter().map(|e| e.name.clone()))
            .chain(manifest.capabilities.process.iter().cloned())
            .collect())
    }
}

/// The story's failing-first test: two loads of the same installed plugin, the second declaring
/// host authority the first never asked for.
#[tokio::test]
async fn a_widened_capability_set_is_refused_at_the_next_load() {
    let store = Store::new("widen", "honest");
    store.load().await.expect("the installed manifest loads");

    // The plugin is upgraded in place. Its operations are identical; only what it asks the host
    // to do on its behalf has grown.
    store.set_mode("widens-capabilities");

    let err = store.load().await.expect_err(
        "a manifest that widens its declared capabilities must not be adopted silently at the \
         next load",
    );
    assert!(
        err.contains("`process` gains `kubectl`"),
        "the refusal must name what widened, not merely that something did: {err}"
    );
    assert!(
        err.contains("`secrets` gains `KUBECONFIG`"),
        "every widening is named, not just the first: {err}"
    );
    assert!(
        err.contains("platform"),
        "the refusal names the plugin: {err}"
    );
}

/// The same widening down the path `flux plugin call` actually takes. `load_plugin_tools` is never
/// called here: the CLI spawns, fetches the manifest itself, and hands it to `with_manifest`, which
/// sets `self.grants = m.capabilities.clone()` — so a check that lived only in the projection helper
/// would leave the widened set enforced for the dispatched op, on the one command an operator uses
/// to poke a plugin by hand.
#[tokio::test]
async fn a_widened_capability_set_is_refused_on_the_plugin_call_path() {
    let store = Store::new("call", "honest");
    store
        .call_path_manifest()
        .await
        .expect("the installed manifest answers");

    store.set_mode("widens-capabilities");

    let err = store.call_path_manifest().await.expect_err(
        "`flux plugin call` must not adopt a widened capability set either — it installs the \
         fetched manifest verbatim via `with_manifest`",
    );
    assert!(err.contains("`process` gains `kubectl`"), "{err}");
    assert!(err.contains("`secrets` gains `KUBECONFIG`"), "{err}");
}

/// The grant has to cover everything `with_manifest` installs, not just `PluginCapabilities`.
/// `ensure_http_host_allowed` admits a host through `http_hosts` **or** `endpoint_allows_host`, so
/// a plugin already granted `http: true` can widen its reachable hosts by adding an `EndpointSpec`
/// while its capability set stays byte-identical. Driven down the `plugin call` path, where the
/// endpoints are installed straight onto the caps.
#[tokio::test]
async fn reach_added_through_an_endpoint_is_a_widening_too() {
    let store = Store::new("endpoint", "http-granted");
    store
        .call_path_manifest()
        .await
        .expect("the installed manifest answers");

    // Same `capabilities` in both modes — only the endpoint list moves.
    store.set_mode("http-granted-plus-endpoint");

    let err = store
        .call_path_manifest()
        .await
        .expect_err("an endpoint is a second egress surface, and gaining one widens reach");
    assert!(
        err.contains("`endpoints` gains or re-points `vendor`"),
        "the refusal names the endpoint that appeared: {err}"
    );
    assert!(
        !err.contains("`http_hosts`"),
        "nothing about the capability set changed, and the message must not pretend otherwise: \
         {err}"
    );
}

/// The other half of the posture: the grant has to be *on record* for a later load to be measured
/// against it, and a first load is what puts it there.
#[tokio::test]
async fn the_first_load_records_the_declared_set_as_the_grant_of_record() {
    let store = Store::new("record", "discloses");
    assert!(
        !store.descriptor_text().contains("[grant"),
        "a freshly installed descriptor has no grant on record yet"
    );

    store.load().await.expect("the installed manifest loads");

    let text = store.descriptor_text();
    assert!(
        text.contains("[grant.capabilities]"),
        "the first load records what the plugin declared: {text}"
    );
    assert!(
        text.contains("connectors.example.com"),
        "the recorded grant is the declaration itself, readable by the operator: {text}"
    );
}

/// Composition with the refresh rules, which treat a *surrender* as accepted-and-ignored: the grant
/// of record is a ceiling, so a plugin that declares less still loads, and one that returns to what
/// it was already granted still loads. Only asking for **more** is refused.
#[tokio::test]
async fn a_narrowed_declaration_loads_and_leaves_the_ceiling_where_it_was() {
    let store = Store::new("narrow", "discloses");
    store.load().await.expect("the installed manifest loads");

    // Narrower: no `http`, no host allowlist at all.
    store.set_mode("honest");
    store
        .load()
        .await
        .expect("a plugin that asks for less than it was granted still loads");

    // Back to the granted set — inside the ceiling, so still no refusal.
    store.set_mode("discloses");
    store
        .load()
        .await
        .expect("returning to the recorded grant is not a widening");

    // And the ceiling itself never moved.
    assert!(
        store.descriptor_text().contains("connectors.example.com"),
        "a narrowing does not rewrite the grant of record"
    );
}
