//! C-403 — the endpoint broker is a **second** plugin-response ingest surface.
//!
//! C-312 put the credential boundary where `call_with_host` returns on the projected-tool path and
//! on `flux plugin call`. Two other call sites reach `call_with_host`, and both live here in L5:
//! [`HostProviderInvoker::discover`] and `HostCredentialReader::read`. The boundary's scope
//! statement excused exactly one class — a host-dispatched `internal: true` op — which does not
//! describe either of them.
//!
//! These tests drive the same `platform_plugin` fixture C-312 uses, through the broker's fan-out
//! rather than through a tool. The fixture is hostile on demand for the same reason it is there:
//! "the credential is not in the candidate list" passes trivially when the credential was never
//! sent.
//!
//! **`secret.read` is deliberately not tested for refusal**, because it must not refuse — its whole
//! purpose is to hand a credential value to host code. The reason is recorded at the call site in
//! `broker.rs`, which is where a future reader will be tempted to "fix" it.

use std::sync::Arc;

use codewandler_flux_capabilities::{
    HostProviderInvoker, PluginRegistry, ProviderEntry, ProviderInvoker,
};
use flux_plugin::{load_plugin_tools, PluginDescriptor, SystemHostCaps};
use serde_json::Value;

/// The vendor credential `platform_plugin` holds, kept in step with the fixture by
/// [`the_fixture_and_this_test_agree_on_the_credential`].
///
/// Joined at compile time (C-325) so a forge's secret scanner finds no credential in this file.
const VENDOR_TOKEN: &str = concat!("xoxb", "-3141592653-2718281828-abcdefghijklmnopqrstuvwx");
/// The prefix-less 40-character form, identified only by the property name it sits under.
const UNMARKED_VENDOR_SECRET: &str = "wJalrXUtnFEMI0K7MDENGbPxRfiCYEXAMPLEKEY0";

/// The product the fixture declares in its manifest `discovers`.
const PRODUCT: &str = "zendesk";

/// The `platform_plugin` fixture binary.
///
/// `CARGO_BIN_EXE_*` is set only for the package that declares the bin, and `platform_plugin`
/// belongs to `flux-plugin` — so derive the path from this test binary instead: an integration test
/// runs out of `<target>/<profile>/deps/`, and a workspace bin lands in `<target>/<profile>/`.
/// Deriving it rather than hard-coding `target/debug` is what makes this follow a custom
/// `CARGO_TARGET_DIR`, and a stale binary from *another* checkout's target dir is exactly the trap
/// a hard-coded path walks into.
fn platform_plugin_bin() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("this test binary has a path");
    let profile = exe
        .parent()
        .and_then(std::path::Path::parent)
        .expect("a test binary lives at <target>/<profile>/deps/<name>");
    let bin = profile.join(format!("platform_plugin{}", std::env::consts::EXE_SUFFIX));
    assert!(
        bin.exists(),
        "the `platform_plugin` fixture is missing at {}. It is a binary of the `flux-plugin` \
         package, so run this test as part of `cargo test --workspace` (or `cargo build -p \
         codewandler-flux-plugin` first) — a `-p codewandler-flux-capabilities` run does not build \
         another package's binaries.",
        bin.display()
    );
    bin
}

/// A temp dir that removes itself on drop, so a failing assertion cannot leak it.
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "flux-c403-{tag}-{}-{:?}",
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

/// The fixture registered as a discovery provider, plus the mode file deciding what it answers.
struct Provider {
    _dir: TempDir,
    mode_file: std::path::PathBuf,
    registry: Arc<PluginRegistry>,
}

impl Provider {
    /// Load `platform_plugin` in `mode` and register it as a provider named `platform`.
    ///
    /// The mode is written **before** the load because the manifest is fetched once, at load: the
    /// `local-discover` control needs its (undeclared) `endpoint.discover` in the manifest the
    /// broker reads, not just in the response.
    async fn load(tag: &str, mode: &str) -> Self {
        let dir = TempDir::new(tag);
        let mode_file = dir.0.join("mode");
        std::fs::write(&mode_file, mode).unwrap();
        let descriptor = PluginDescriptor {
            program: platform_plugin_bin().to_string_lossy().into_owned(),
            args: vec![mode_file.to_string_lossy().into_owned()],
            ..Default::default()
        };
        let system =
            flux_system::System::new(flux_system::Workspace::new(std::env::temp_dir()).unwrap());
        let caps_system = Arc::new(flux_system::System::new(
            flux_system::Workspace::new(std::env::temp_dir()).unwrap(),
        ));
        let loaded = load_plugin_tools(&system, "platform", &descriptor, move |manifest| {
            Arc::new(SystemHostCaps::new(caps_system).with_grants(manifest.capabilities.clone()))
        })
        .await
        .expect("the fixture loads");
        let registry = Arc::new(PluginRegistry::new());
        registry.register(
            "platform",
            ProviderEntry {
                manifest: Arc::new(loaded.manifest.clone()),
                host: loaded.host.clone(),
                caps: loaded.caps.clone(),
            },
        );
        // `loaded` owns the projected tools; the host handle is what keeps the subprocess alive and
        // it is cloned into the registry above, so dropping `loaded` here is safe.
        Self {
            _dir: dir,
            mode_file,
            registry,
        }
    }

    fn set_mode(&self, mode: &str) {
        std::fs::write(&self.mode_file, mode).unwrap();
    }

    /// Fan a discovery query at the fixture through the production invoker — the exact seam
    /// `EndpointBroker::discover` drives.
    async fn discover(&self) -> Result<String, String> {
        let invoker = HostProviderInvoker::new(self.registry.clone());
        invoker
            .discover("platform", PRODUCT, &Value::Null, None, None, 10)
            .await
            .map(|candidates| format!("{candidates:?}"))
    }
}

/// The fixture's credential and this file's copy of it must be the same bytes, or every "the
/// credential is absent" assertion below is vacuous.
#[tokio::test]
async fn the_fixture_and_this_test_agree_on_the_credential() {
    // `local-discover` declares no `platform` sourcing, so nothing filters the response: whatever
    // the fixture is willing to emit comes back verbatim. Round-tripping the constant proves the
    // two files agree.
    let provider = Provider::load("agree", "local-discover").await;
    let rendered = provider.discover().await.expect("discovery succeeds");
    assert!(
        rendered.contains(VENDOR_TOKEN),
        "the fixture did not emit the credential this test asserts about: {rendered}"
    );
}

// ---------------------------------------------------------------------------
// The failing-first test
// ---------------------------------------------------------------------------

/// **The story's failing-first test.** A platform-sourced `endpoint.discover` whose response
/// carries credential material is REFUSED on the broker's fan-out path, exactly as it is on the
/// projected-tool path.
///
/// Three shapes, chosen to exercise the recognisers independently rather than to pile on examples:
/// a vendor-prefixed token in a match *reason*, a prefix-less opaque value under a secret-naming
/// *label*, and the error frame. Each asserts two things: the call is an error (a redacted-but-
/// successful candidate list would satisfy "no credential in the registry" while telling the
/// operator nothing), and the credential is absent from the refusal itself — a refusal that quoted
/// the value would be the leak it exists to prevent.
///
/// The candidates are discarded **whole**: a weak endpoint reference that arrived alongside
/// credential material is not sanitised and committed, because the boundary's claim is that the
/// deployment crossed it, not that one field needed masking.
#[tokio::test]
async fn a_platform_sourced_discovery_carrying_a_vendor_credential_is_refused() {
    let provider = Provider::load("refused", "honest").await;

    for (mode, needle) in [
        ("leak-discover", VENDOR_TOKEN),
        ("leak-discover-unmarked", UNMARKED_VENDOR_SECRET),
        ("leak-discover-error", VENDOR_TOKEN),
    ] {
        provider.set_mode(mode);
        let outcome = provider.discover().await;
        let refusal = match outcome {
            Err(refusal) => refusal,
            Ok(candidates) => panic!(
                "mode `{mode}`: a credential-bearing discovery was accepted, not refused: \
                 {candidates}"
            ),
        };
        assert!(
            !refusal.contains(needle),
            "mode `{mode}`: the refusal leaked the credential: {refusal}"
        );
        assert!(
            !refusal.contains("example.zendesk.com"),
            "mode `{mode}`: a refused discovery still forwarded its candidates: {refusal}"
        );
    }
}

/// The positive control: an honest deployment's candidates still reach the broker. A boundary that
/// refused every discovery would pass the test above and make the seam unusable.
#[tokio::test]
async fn an_honest_platform_sourced_discovery_still_flows() {
    let provider = Provider::load("flows", "honest").await;
    let rendered = provider
        .discover()
        .await
        .expect("honest discovery succeeds");
    assert!(
        rendered.contains("example.zendesk.com"),
        "the honest candidate did not survive: {rendered}"
    );
}

/// The other direction, and the reason the boundary is a declaration rather than a filter: a
/// provider whose `endpoint.discover` is NOT platform-sourced keeps its existing posture.
///
/// This is the case that matters for what ships today — `kubernetes.endpoint.discover` is declared
/// with `read_op_typed`, so it carries no `platform` declaration and this change does not alter one
/// byte of its behaviour. A boundary that had quietly become a content filter on all provider
/// output would break every discovery provider in the pack and would still pass the test above.
#[tokio::test]
async fn a_provider_that_is_not_platform_sourced_is_not_refused() {
    let provider = Provider::load("scoped", "local-discover").await;
    let rendered = provider
        .discover()
        .await
        .expect("an undeclared provider is not subject to the boundary");
    assert!(rendered.contains("example.zendesk.com"), "{rendered}");
}
