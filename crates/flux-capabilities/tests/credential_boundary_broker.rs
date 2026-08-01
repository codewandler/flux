//! C-403 — the endpoint broker is a **second** plugin-response ingest surface.
//!
//! C-312 put the credential boundary where `call_with_host` returns on the projected-tool path and
//! on `flux plugin call`. Two of the tree's remaining `call_with_host` sites live here in L5 —
//! [`HostProviderInvoker::discover`] and `HostCredentialReader::read` — and the boundary's scope
//! statement excused exactly one class, a host-dispatched `internal: true` op, which describes
//! neither. (The third unlisted site was `flux plugin call --dry-run`'s `plugin.validate`
//! preflight, which the carve-out did cover until C-404 removed the carve-out. The census is no
//! longer prose: `flux-codegate`'s
//! `every_plugin_response_ingest_site_is_in_the_credential_boundary_census` enforces it.)
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
/// The deployment session bearer the fixture echoes back in its `leak-discover-bearer` mode.
/// Shapeless on purpose — see the fixture, and
/// [`the_session_bearer_is_refused_only_by_a_redactor_that_has_it_registered`].
const SESSION_BEARER: &str = "connectors-session-bearer-0a1b2c3d4e5f";

/// The product the fixture declares in its manifest `discovers`.
const PRODUCT: &str = "zendesk";

/// The `platform_plugin` fixture binary, built on demand when the current run has not produced it.
///
/// `CARGO_BIN_EXE_*` is set only for the package that declares the bin, and `platform_plugin`
/// belongs to `flux-plugin` — so derive the path from this test binary instead: an integration test
/// runs out of `<target>/<profile>/deps/`, and a workspace bin lands in `<target>/<profile>/`.
/// Deriving it rather than hard-coding `target/debug` is what makes this follow a custom
/// `CARGO_TARGET_DIR`, and a stale binary from *another* checkout's target dir is exactly the trap
/// a hard-coded path walks into.
///
/// Deriving the path does not make the fixture *exist*. `cargo test --workspace` compiles every
/// member's binaries before it runs any test binary, so the fixture is simply there; but CI's
/// Postgres job runs this package on its own (`cargo test -p codewandler-flux-events -p
/// codewandler-flux-capabilities --features postgres`), and a `-p` run does not build another
/// package's binaries. That left the fixture absent and all five tests below red.
///
/// So build it here rather than skip. **Skipping is not an option worth having**: these assert that
/// a vendor credential is refused at the broker, and a security test that quietly no-ops when its
/// fixture is missing is worse than no test at all. A build that fails is still a loud failure.
///
/// The build is deliberately conditional on the binary being absent. When cargo built the fixture
/// for this run it is cargo, not this function, that decides freshness; re-running a nested `cargo
/// build -p codewandler-flux-plugin` under `cargo test --workspace` would resolve features over
/// just the `flux-plugin` subtree rather than the whole workspace, and the two resolutions would
/// rebuild over each other on every dev-loop test run.
fn platform_plugin_bin() -> std::path::PathBuf {
    /// Built at most once per test process — the five tests below run on parallel threads.
    static BIN: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    BIN.get_or_init(|| {
        let exe = std::env::current_exe().expect("this test binary has a path");
        let profile_dir = exe
            .parent()
            .and_then(std::path::Path::parent)
            .expect("a test binary lives at <target>/<profile>/deps/<name>");
        let bin = profile_dir.join(format!("platform_plugin{}", std::env::consts::EXE_SUFFIX));
        if !bin.exists() {
            build_platform_plugin(profile_dir, &bin);
        }
        bin
    })
    .clone()
}

/// Build the `platform_plugin` fixture into `profile_dir`, or panic saying why it could not be.
///
/// `--target-dir` is pinned to the directory the fixture path was derived from, so the build cannot
/// land somewhere other than where the caller looks; `--profile` is recovered from that same path
/// (cargo's `dev` profile writes to a directory named `debug`, every other profile writes to a
/// directory carrying its own name). `CARGO` is the cargo that launched this test — falling back to
/// `PATH` keeps a directly-executed test binary working. The cwd is pinned to this package's
/// manifest directory so workspace discovery does not depend on where the binary was invoked from.
fn build_platform_plugin(profile_dir: &std::path::Path, bin: &std::path::Path) {
    let target_dir = profile_dir
        .parent()
        .expect("a profile directory lives under the target directory");
    let profile = match profile_dir.file_name().and_then(std::ffi::OsStr::to_str) {
        Some("debug") => "dev",
        Some(name) => name,
        None => panic!(
            "the profile directory {} has no name to recover a cargo profile from",
            profile_dir.display()
        ),
    };
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());

    let output = std::process::Command::new(&cargo)
        .args([
            "build",
            "-p",
            "codewandler-flux-plugin",
            "--bin",
            "platform_plugin",
            "--profile",
            profile,
        ])
        .arg("--target-dir")
        .arg(target_dir)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap_or_else(|err| {
            panic!(
                "could not run `{cargo} build -p codewandler-flux-plugin --bin platform_plugin` to \
                 produce the fixture these credential-boundary tests drive: {err}"
            )
        });

    assert!(
        output.status.success(),
        "building the `platform_plugin` fixture failed ({}):\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        bin.exists(),
        "`cargo build -p codewandler-flux-plugin --bin platform_plugin --profile {profile}` \
         reported success but left no fixture at {}",
        bin.display()
    );
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
        self.discover_with(HostProviderInvoker::new(self.registry.clone()))
            .await
    }

    /// The same query through a caller-built invoker, so a test can choose which redactor the
    /// boundary is evaluated against.
    async fn discover_with(&self, invoker: HostProviderInvoker) -> Result<String, String> {
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

/// **The one production behaviour `HostProviderInvoker::with_redactor` exists for.**
///
/// The deployment's own session bearer is the single secret flux holds on this seam. Echoed back
/// inside a vendor response it has no business being there — and it is the case *no shape heuristic
/// can see*: the fixture emits it as free prose in a match `reason`, with no vendor prefix and no
/// secret-naming property above it. Only the redactor's registered-value pass can refuse it, and
/// that pass can only fire on a redactor that carries the session's registrations.
///
/// Both halves are asserted, and the first is what gives the second its meaning:
///
/// 1. with the `HostProviderInvoker::new` default — a **fresh** redactor, nothing registered — the
///    response is accepted. That is the control proving the refusal below is the registered pass
///    doing the work rather than a shape recogniser that would have caught it either way;
/// 2. with the session redactor installed via `with_redactor`, the same response is refused.
///
/// Delete the `with_redactor` call in `flux-cli`'s `assemble_integrations` and the production path
/// silently becomes case 1. This test is what makes that a red gate rather than a quiet downgrade.
#[tokio::test]
async fn the_session_bearer_is_refused_only_by_a_redactor_that_has_it_registered() {
    let provider = Provider::load("bearer", "leak-discover-bearer").await;

    let fresh = provider
        .discover_with(HostProviderInvoker::new(provider.registry.clone()))
        .await
        .expect("a fresh redactor has nothing registered, so nothing here can see the bearer");
    assert!(
        fresh.contains(SESSION_BEARER),
        "the fixture did not emit the bearer this test asserts about, so the refusal below would \
         prove nothing: {fresh}"
    );

    let redactor = flux_secret::Redactor::new();
    redactor
        .try_add_secret(SESSION_BEARER)
        .expect("the fixture bearer must clear the registration floor");
    let refusal = provider
        .discover_with(HostProviderInvoker::new(provider.registry.clone()).with_redactor(redactor))
        .await
        .expect_err("the session bearer coming back must be refused");
    assert!(
        !refusal.contains(SESSION_BEARER),
        "the refusal leaked the bearer: {refusal}"
    );
    assert!(
        !refusal.contains("example.zendesk.com"),
        "a refused discovery still forwarded its candidates: {refusal}"
    );
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
