//! C-311 — vendor-host disclosure at approval: show what an op reaches when flux is not dialing.
//!
//! The connectors seam's accepted design has the deployment execute the vendor call, so flux sends
//! `{op, args}` to the deployment and never sees a vendor URL. `guard_url_scoped` therefore only
//! ever sees the deployment's own base URL, and flux's per-vendor egress allowlist stops
//! constraining *which vendor* is reached. This file holds the compensating control: the operator
//! is told which vendor an operation reaches **at the moment they are asked to approve it**.
//!
//! Every test drives `platform_plugin`, the same hostile-on-demand deployment fixture C-312 and
//! C-403 use, in the `discloses*` modes this story added.
//!
//! The disclosure is a *declaration*, so most of what follows is about the bounds on it: a manifest
//! may not name a host outside its own declared allowlist, may not spell a URL where a host belongs,
//! and may not shed or re-point a disclosure across a refresh.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use flux_plugin::{load_plugin_tools, LoadedPlugin, PluginDescriptor, SystemHostCaps};
use flux_runtime::{
    ApprovalChoice, Approver, Executor, PermissionManager, Tool, ToolContext, ToolRegistry,
};
use serde_json::json;

/// The vendor the fixture's deployment says it reaches. Kept in step with the fixture by
/// `the_fixture_and_the_test_agree_on_the_vendor`, which fails if the two drift.
const VENDOR_HOST: &str = "api.zendesk.com";
/// The credential `platform_plugin` tries to smuggle out inside a URL-shaped "host" declaration.
/// Joined at compile time (C-325) so a forge's secret scanner finds no credential in this file.
const VENDOR_TOKEN: &str = concat!("xoxb", "-3141592653-2718281828-abcdefghijklmnopqrstuvwx");

fn test_system() -> flux_system::System {
    flux_system::System::new(flux_system::Workspace::new(std::env::temp_dir()).unwrap())
}

/// A temp dir that removes itself on drop, so a failing assertion cannot leak it.
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "flux-c311-{tag}-{}-{:?}",
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

/// Everything an approval prompt was handed, in the order it was asked.
///
/// This stands in for the two interactive surfaces rather than duplicating them: `StdinApprover`
/// (the plain CLI / REPL) prints `subjects` beside the op name, and `flux-tui`'s approval sheet
/// lists `subjects` as the sheet body. Both read this exact slice, so asserting on it asserts on
/// what a human is shown.
#[derive(Default)]
struct RecordingApprover {
    seen: Mutex<Vec<(String, Vec<String>)>>,
}

#[async_trait]
impl Approver for RecordingApprover {
    async fn request(
        &self,
        tool: &str,
        subjects: &[String],
        _intents: &flux_spec::IntentSet,
    ) -> ApprovalChoice {
        self.seen
            .lock()
            .unwrap()
            .push((tool.to_string(), subjects.to_vec()));
        // Deny: this file is about what the operator is *told*, and denying keeps the subprocess
        // out of every assertion below.
        ApprovalChoice::Deny
    }
}

impl RecordingApprover {
    /// The subjects the prompt showed for `tool`, or a panic naming what it did see.
    fn subjects_for(&self, tool: &str) -> Vec<String> {
        let seen = self.seen.lock().unwrap();
        seen.iter()
            .find(|(name, _)| name == tool)
            .map(|(_, subjects)| subjects.clone())
            .unwrap_or_else(|| panic!("no approval was requested for `{tool}`; saw {seen:?}"))
    }
}

/// A loaded `platform` plugin plus an executor whose approver records every prompt.
struct Fixture {
    _dir: TempDir,
    mode_file: std::path::PathBuf,
    loaded: LoadedPlugin,
    approver: Arc<RecordingApprover>,
    executor: Executor,
}

impl Fixture {
    async fn load_in_mode(tag: &str, mode: &str) -> flux_core::Result<Self> {
        let dir = TempDir::new(tag);
        let mode_file = dir.0.join("mode");
        std::fs::write(&mode_file, mode).unwrap();
        let descriptor = PluginDescriptor {
            program: env!("CARGO_BIN_EXE_platform_plugin").to_string(),
            args: vec![mode_file.to_string_lossy().into_owned()],
            ..Default::default()
        };
        let system = test_system();
        let caps_system = Arc::new(test_system());
        let loaded = load_plugin_tools(&system, "platform", &descriptor, move |manifest| {
            Arc::new(SystemHostCaps::new(caps_system).with_grants(manifest.capabilities.clone()))
        })
        .await?;

        let mut registry = ToolRegistry::new();
        for tool in &loaded.tools {
            registry
                .try_register_from("plugin:platform", tool.clone())
                .expect("the projected catalog registers");
        }
        let approver = Arc::new(RecordingApprover::default());
        // No permission rules at all, so every op reaches the approval gate — the state a fresh
        // session is in, and the only state in which a disclosure is worth anything.
        let executor = Executor::new(
            registry,
            PermissionManager::new(),
            approver.clone(),
            ToolContext::new(Arc::new(test_system())),
        );
        Ok(Self {
            _dir: dir,
            mode_file,
            loaded,
            approver,
            executor,
        })
    }

    async fn load(tag: &str, mode: &str) -> Self {
        Self::load_in_mode(tag, mode)
            .await
            .unwrap_or_else(|e| panic!("the fixture loads in mode `{mode}`: {e}"))
    }

    fn set_mode(&self, mode: &str) {
        std::fs::write(&self.mode_file, mode).unwrap();
    }

    /// Ask for `op`, driving the real dispatch path so the approval prompt is the production one.
    async fn ask(&self, op: &str) {
        let result = self.executor.dispatch(op, json!({ "text": "help" })).await;
        assert!(
            result.is_error,
            "the recording approver denies, so a dispatch must not have succeeded: {}",
            result.content
        );
    }

    fn tool(&self, name: &str) -> Arc<dyn Tool> {
        self.loaded
            .tools
            .iter()
            .find(|t| t.spec().name == name)
            .unwrap_or_else(|| panic!("no such projected tool: {name}"))
            .clone()
    }
}

/// The fixture's declared vendor and this file's copy of it must be the same bytes, or the
/// assertions below are asserting about nothing.
#[tokio::test]
async fn the_fixture_and_the_test_agree_on_the_vendor() {
    let fx = Fixture::load("agree", "discloses").await;
    let hosts = &fx.loaded.manifest.capabilities.http_hosts;
    assert!(
        hosts.iter().any(|h| h == VENDOR_HOST),
        "the fixture's allowlist does not contain the vendor this test asserts about: {hosts:?}"
    );
}

// ---------------------------------------------------------------------------
// The failing-first test
// ---------------------------------------------------------------------------

/// **The story's failing-first test.** An approval request for an operation whose manifest declares
/// a vendor host carries that host in what the approver sees.
///
/// It fails before this story because the declaration never reaches the approval path: the prompt
/// says `platform.dispatch` and nothing else, while the call reaches `api.zendesk.com` through a
/// deployment `guard_url_scoped` cannot see past. An operator approving that is approving without
/// the material fact.
///
/// The assertion is on the slice both interactive approvers render — see [`RecordingApprover`] —
/// and it is driven through `Executor::dispatch`, so it is the production gate that produces it and
/// not a hand-assembled prompt.
#[tokio::test]
async fn an_approval_for_a_platform_sourced_op_discloses_the_vendor_it_reaches() {
    let fx = Fixture::load("discloses", "discloses").await;
    fx.ask("platform.dispatch").await;

    let subjects = fx.approver.subjects_for("platform.dispatch");
    assert!(
        subjects.iter().any(|s| s.contains(VENDOR_HOST)),
        "the operator was asked to approve a call to `{VENDOR_HOST}` without being told: {subjects:?}"
    );
    // And the disclosure says who dials. `network:…` would claim the opposite of the fact.
    assert!(
        subjects
            .iter()
            .any(|s| s == &format!("platform-reaches:{VENDOR_HOST}")),
        "the disclosure must name the platform as the dialer: {subjects:?}"
    );
    // The op identity the operator's permission rules key on is untouched — the disclosure is
    // additive, never a replacement.
    assert!(
        subjects.iter().any(|s| s == "platform.dispatch"),
        "the op subject was lost: {subjects:?}"
    );
}

// ---------------------------------------------------------------------------
// Silence is a disclosure too
// ---------------------------------------------------------------------------

/// "Unknown destination" and "no destination" must not look identical, and neither may look like
/// silence. All three ops here come from one load of the `discloses` manifest.
#[tokio::test]
async fn an_undeclared_destination_never_renders_as_no_destination() {
    let fx = Fixture::load("tri-state", "discloses").await;
    for op in [
        "platform.dispatch",
        "platform.activate",
        "platform.endpoint.discover",
    ] {
        fx.ask(op).await;
    }

    let vendor = fx.approver.subjects_for("platform.dispatch");
    let local = fx.approver.subjects_for("platform.activate");
    let unknown = fx.approver.subjects_for("platform.endpoint.discover");

    let disclosure = |subjects: &[String]| -> String {
        subjects
            .iter()
            .find(|s| s.starts_with("platform-reaches:"))
            .unwrap_or_else(|| panic!("a platform-sourced op disclosed nothing: {subjects:?}"))
            .clone()
    };
    let vendor = disclosure(&vendor);
    let local = disclosure(&local);
    let unknown = disclosure(&unknown);

    assert_eq!(vendor, format!("platform-reaches:{VENDOR_HOST}"));
    assert_eq!(local, "platform-reaches:none");
    assert_eq!(unknown, "platform-reaches:UNDECLARED");
    assert_ne!(
        local, unknown,
        "`no destination` and `unknown destination` rendered identically"
    );
}

/// The other direction, and the reason this is a declaration rather than a blanket annotation: an
/// op flux dials itself gains nothing. Its destination is already bound by `guard_url_scoped` and
/// named by its own `network.fetch` authority, and a second unverifiable story beside an enforced
/// one is worse than none. This is also what keeps every existing plugin's permission subjects —
/// and therefore every operator's existing allow rules — byte-identical.
#[tokio::test]
async fn an_op_that_is_not_platform_sourced_discloses_nothing_extra() {
    let fx = Fixture::load("scoped", "discloses").await;
    fx.ask("platform.echo").await;
    assert_eq!(
        fx.approver.subjects_for("platform.echo"),
        vec!["platform.echo".to_string()],
        "a non-platform-sourced op's subjects must be exactly what they were"
    );
}

// ---------------------------------------------------------------------------
// The declaration is re-verified, not trusted
// ---------------------------------------------------------------------------

/// A vendor host outside the manifest's own declared HTTP allowlist is refused at load.
///
/// This is what makes the disclosure worth reading. The `reaches` field is per-operation free text
/// authored by the same untrusted manifest as everything else; bounding it by the manifest-wide
/// allowlist means it can only ever name a host the operator already reviewed at install, and which
/// the approval prompt already renders as a `network.fetch` authority. Without the check, a plugin
/// could disclose a reassuring vendor while its allowlist admitted something else entirely.
#[tokio::test]
async fn a_vendor_host_outside_the_manifests_own_allowlist_is_refused() {
    let err = Fixture::load_in_mode("outside", "discloses-outside-allowlist")
        .await
        .err()
        .expect("a host outside the manifest's own allowlist must be refused")
        .to_string();
    assert!(
        err.contains("allowlist"),
        "the refusal must say what rule it broke: {err}"
    );
    assert!(err.contains("dispatch"), "must name the op: {err}");
}

/// **The redaction-safety criterion, structurally.** The disclosed value is rendered verbatim at an
/// approval prompt, so a manifest that could spell a URL there could put a token on an operator's
/// terminal. The grammar refuses anything but a bare `host`/`host:port`, so the channel does not
/// exist rather than being something a later renderer has to remember to strip — and the refusal
/// itself never quotes the value it rejected, which would have been the same leak in a diagnostic.
#[tokio::test]
async fn a_vendor_host_that_is_a_url_is_refused_without_quoting_it() {
    let err = Fixture::load_in_mode("url", "discloses-a-url")
        .await
        .err()
        .expect("a URL where a host belongs must be refused")
        .to_string();
    assert!(
        !err.contains(VENDOR_TOKEN),
        "the refusal echoed the credential the manifest smuggled in: {err}"
    );
    assert!(
        err.contains("bare"),
        "the refusal must say what shape it wanted: {err}"
    );
}

/// A destination claimed for an operation flux dials itself is refused too. There the enforced
/// answer already exists, and a manifest-authored one beside it would be a decoy an operator could
/// not tell from the real thing.
#[tokio::test]
async fn a_destination_declared_for_an_op_flux_dials_itself_is_refused() {
    let err = Fixture::load_in_mode("no-platform", "discloses-without-platform")
        .await
        .err()
        .expect("a vendor reach without platform sourcing must be refused")
        .to_string();
    assert!(err.contains("platform-sourced"), "{err}");
}

// ---------------------------------------------------------------------------
// The whole-plan surface
// ---------------------------------------------------------------------------

/// The plan prompt renders typed authority requirements and never permission subjects, and an
/// approved plan skips the per-op gate — so a plan-approved batch would otherwise disclose nothing.
/// The destination has to be in the requirement set too.
#[tokio::test]
async fn the_whole_plan_surface_names_the_destination_too() {
    let fx = Fixture::load("plan", "discloses").await;

    let named = |op: &str| -> Vec<String> {
        let tool = fx.tool(op);
        let input = json!({});
        let subjects = tool.permission_subjects(&input);
        tool.authority_requirements(&input, &subjects)
            .expect("a valid authority contract")
            .into_iter()
            .filter(|r| r.action.0 == "network.fetch")
            // Exactly what both plan prompts render: `path`, else `name`, else the resource id.
            .map(|r| {
                r.resource
                    .path
                    .clone()
                    .or_else(|| r.resource.name.clone())
                    .unwrap_or_else(|| r.resource.id.clone())
            })
            .collect()
    };

    assert!(
        named("platform.dispatch").iter().any(|h| h == VENDOR_HOST),
        "the plan preview never names the vendor: {:?}",
        named("platform.dispatch")
    );
    // Silence gets a resource of its own — "this plan reaches somewhere nobody named" is the
    // disclosure, and it is spelled so it cannot collide with a hostname.
    assert!(
        named("platform.endpoint.discover")
            .iter()
            .any(|h| h == "platform-reaches:UNDECLARED"),
        "an undeclared destination vanished from the plan preview: {:?}",
        named("platform.endpoint.discover")
    );
}

// ---------------------------------------------------------------------------
// A refresh is a re-grant
// ---------------------------------------------------------------------------

/// An operation the operator approved knowing it reached `api.zendesk.com` must not keep its name
/// while going silent, or while pointing somewhere else. A refresh re-projects the catalog on the
/// live subprocess, so without this a plugin could disclose honestly at load and re-point itself
/// afterwards under the approval the session is still carrying.
#[tokio::test]
async fn a_refresh_may_not_shed_or_repoint_a_vendor_disclosure() {
    for (mode, needle) in [
        ("sheds-disclosure", "drops its vendor-reach disclosure"),
        ("repoints-disclosure", "re-points its declared vendor host"),
    ] {
        let mut fx = Fixture::load("refresh", "discloses").await;
        fx.set_mode(mode);
        let err = fx
            .loaded
            .refresh()
            .await
            .err()
            .unwrap_or_else(|| panic!("mode `{mode}`: the refresh was accepted"))
            .to_string();
        assert!(err.contains(needle), "mode `{mode}`: {err}");
    }
}
