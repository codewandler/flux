//! C-312 — the credential boundary: prove a vendor credential never enters flux.
//!
//! Every test here drives `platform_plugin`, a fixture standing in for a connector-platform
//! deployment. It holds a vendor credential of its own and, in its `leak-*` modes, sends it. That
//! is the whole design of the fixture: an assertion that a credential is absent from a log passes
//! trivially when the credential was never sent, so the counterparty has to be hostile on demand
//! for the assertion to mean anything.
//!
//! The invariant: on the connectors seam the **deployment** resolves and injects the vendor
//! credential; flux holds exactly one secret — the deployment's own session bearer. A response
//! carrying credential-shaped material is **refused**, not merely redacted.

use std::sync::Arc;

use flux_plugin::{load_plugin_tools, LoadedPlugin, PluginDescriptor, SystemHostCaps};
use flux_runtime::{
    AllowApprover, Executor, PermissionManager, Tool, ToolContext, ToolRegistry, ToolResult,
};
use serde_json::{json, Value};

/// The vendor credential `platform_plugin` holds. Kept in step with the fixture by
/// `the_fixture_and_the_test_agree_on_the_credential`, which fails if they drift.
///
/// Joined at compile time (C-325) so a forge's secret scanner finds no credential in this file.
const VENDOR_TOKEN: &str = concat!("xoxb", "-3141592653-2718281828-abcdefghijklmnopqrstuvwx");
/// The prefix-less 40-character form, identified only by the property name it sits under.
const UNMARKED_VENDOR_SECRET: &str = "wJalrXUtnFEMI0K7MDENGbPxRfiCYEXAMPLEKEY0";
/// The password in the deployment's own DSN.
const DSN_PASSWORD: &str = "hunter2pass";

fn test_system() -> flux_system::System {
    flux_system::System::new(flux_system::Workspace::new(std::env::temp_dir()).unwrap())
}

/// A temp dir that removes itself on drop, so a failing assertion cannot leak it.
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "flux-c312-{tag}-{}-{:?}",
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

/// A loaded `platform` plugin plus the mode file that decides what it answers next.
struct Fixture {
    _dir: TempDir,
    mode_file: std::path::PathBuf,
    loaded: LoadedPlugin,
    ctx: ToolContext,
}

impl Fixture {
    async fn load(tag: &str) -> Self {
        Self::load_in_mode(tag, "honest")
            .await
            .expect("the fixture loads")
    }

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
        Ok(Self {
            _dir: dir,
            mode_file,
            loaded,
            ctx: ToolContext::new(Arc::new(test_system())),
        })
    }

    fn set_mode(&self, mode: &str) {
        std::fs::write(&self.mode_file, mode).unwrap();
    }

    fn tool(&self, name: &str) -> Arc<dyn Tool> {
        self.loaded
            .tools
            .iter()
            .find(|t| t.spec().name == name)
            .unwrap_or_else(|| panic!("no such projected tool: {name}"))
            .clone()
    }

    /// Call an op straight through its projected [`Tool`] — the ingest seam itself, with no
    /// executor in the way, so a failure is unambiguously the boundary's.
    async fn call(&self, name: &str, params: Value) -> ToolResult {
        self.tool(name)
            .execute(&self.ctx, params)
            .await
            .expect("the tool call itself does not error out")
    }
}

/// The fixture's credential and this file's copy of it must be the same bytes, or every "the
/// credential is absent" assertion below is vacuous.
#[tokio::test]
async fn the_fixture_and_the_test_agree_on_the_credential() {
    let fx = Fixture::load("agree").await;
    fx.set_mode("leak");
    // `echo` is not platform-sourced, so nothing filters it: whatever the fixture is willing to
    // emit comes back verbatim. Round-tripping the constant proves the two files agree.
    let echoed = fx
        .call("platform.echo", json!({ "text": VENDOR_TOKEN }))
        .await;
    assert!(!echoed.is_error, "{}", echoed.content);
    assert!(
        echoed.content.contains(VENDOR_TOKEN),
        "the fixture did not echo the credential this test asserts about: {}",
        echoed.content
    );
}

// ---------------------------------------------------------------------------
// The failing-first test
// ---------------------------------------------------------------------------

/// **The story's failing-first test.** A platform-sourced operation whose response carries
/// credential-shaped material is REFUSED, not merely redacted.
///
/// Refusal, not redaction, is the point: redaction hides a leak from the model, refusal says the
/// boundary was crossed. So this asserts three separate things, and each is load-bearing:
///
/// 1. the call is an **error** — a redacted-but-successful result would satisfy "no credential in
///    the log" while telling the operator nothing;
/// 2. the credential is not in the result — including not in the refusal message, which would be
///    the leak the refusal exists to prevent;
/// 3. the *payload* is gone, not just the token — a refused response is discarded whole.
///
/// Four shapes, chosen to cover the recognisers independently rather than to pile on examples: a
/// vendor-prefixed token, a prefix-less opaque value identified only by its property name, a URL
/// authority password, and an error frame. Remove the ingest check and all four go red.
#[tokio::test]
async fn a_platform_sourced_response_carrying_a_vendor_credential_is_refused() {
    let fx = Fixture::load("refused").await;

    for (mode, needle) in [
        ("leak", VENDOR_TOKEN),
        ("leak-unmarked", UNMARKED_VENDOR_SECRET),
        ("leak-dsn", DSN_PASSWORD),
        ("leak-error", VENDOR_TOKEN),
    ] {
        fx.set_mode(mode);
        let result = fx
            .call("platform.dispatch", json!({ "text": "help" }))
            .await;
        assert!(
            result.is_error,
            "mode `{mode}`: a credential-bearing response was accepted, not refused: {}",
            result.content
        );
        assert!(
            !result.content.contains(needle),
            "mode `{mode}`: the refusal leaked the credential: {}",
            result.content
        );
        assert!(
            result.view.as_deref().is_none_or(|v| !v.contains(needle)),
            "mode `{mode}`: the credential survived in the result view"
        );
        // The response is discarded whole, not scrubbed and forwarded.
        assert!(
            !result.content.contains("4711"),
            "mode `{mode}`: a refused response was still forwarded: {}",
            result.content
        );
    }
}

/// The other direction, and the reason the boundary is a declaration rather than a filter: an op
/// that is NOT platform-sourced keeps its existing posture. Its output is redacted at dispatch,
/// never refused. A boundary that quietly became a content filter on all plugin output would break
/// every existing plugin and would pass the test above.
#[tokio::test]
async fn an_op_that_is_not_platform_sourced_is_not_refused() {
    let fx = Fixture::load("scoped").await;
    let result = fx
        .call("platform.echo", json!({ "text": VENDOR_TOKEN }))
        .await;
    assert!(
        !result.is_error,
        "the boundary must apply only where a manifest declares it: {}",
        result.content
    );
    assert!(result.content.contains(VENDOR_TOKEN));
}

/// And the boundary is not a blanket refusal of anything a vendor might say: an ordinary ticket
/// payload still flows. A boundary that refuses everything is a boundary somebody turns off.
#[tokio::test]
async fn an_ordinary_platform_sourced_response_still_flows() {
    let fx = Fixture::load("flows").await;
    let result = fx
        .call(
            "platform.dispatch",
            json!({ "text": "Password reset help" }),
        )
        .await;
    assert!(!result.is_error, "{}", result.content);
    assert!(result.content.contains("4711"), "{}", result.content);
    assert!(
        result.content.contains("Password reset help"),
        "a ticket subject containing the word `password` is not a credential: {}",
        result.content
    );
}

// ---------------------------------------------------------------------------
// The manifest half
// ---------------------------------------------------------------------------

/// A platform-sourced op declaring `secret_purposes` is refused **at load**, naming the op.
///
/// The two declarations say opposite things about who resolves the credential, and the combination
/// is not merely redundant: a non-empty `secret_purposes` is what gives the projected op
/// `AccessKind::Secret` and a `secret.read` authority requirement, i.e. the authority to be handed
/// a raw credential — the exact thing the seam exists to make impossible.
#[tokio::test]
async fn a_platform_sourced_op_declaring_secret_purposes_is_refused_at_load() {
    let error = Fixture::load_in_mode("secret-purposes", "declares-secret-purposes")
        .await
        .err()
        .expect("the manifest must be refused")
        .to_string();
    assert!(error.contains("dispatch"), "must name the op: {error}");
    assert!(
        error.contains("secret_purposes") || error.contains("secret purposes"),
        "must name what is wrong: {error}"
    );
    // The honest manifest is unaffected — the refusal is about the contradiction, not the field.
    assert!(Fixture::load_in_mode("secret-purposes-ok", "honest")
        .await
        .is_ok());
}

/// A refresh may not let an op shed the declaration that installed its boundary.
///
/// Same class as C-310's `a_refresh_cannot_widen_the_granted_capabilities`: an op keeping its name
/// across a refresh may not become a differently-gated op, because policy rules, permission
/// subjects and session grants all key on that name. Here the gate is the credential boundary
/// itself, so shedding it would be the cleanest possible escape — answer `manifest` once with the
/// declaration to get loaded, once without it to get the credential through.
#[tokio::test]
async fn a_refresh_cannot_shed_an_ops_platform_sourcing() {
    let mut fx = Fixture::load("sheds").await;
    fx.set_mode("sheds-platform");
    let error = fx
        .loaded
        .refresh()
        .await
        .err()
        .expect("a refresh that sheds platform sourcing must be refused")
        .to_string();
    assert!(error.contains("dispatch"), "must name the op: {error}");
    assert!(
        error.contains("platform"),
        "must say what was shed: {error}"
    );
    // All-or-nothing: the load-time boundary is still in force after the refused refresh.
    fx.set_mode("leak");
    let result = fx.call("platform.dispatch", json!({})).await;
    assert!(
        result.is_error && !result.content.contains(VENDOR_TOKEN),
        "a refused refresh left the boundary off: {}",
        result.content
    );
}

// ---------------------------------------------------------------------------
// The activation half
// ---------------------------------------------------------------------------

/// The activation path returns **a URL for a human** and never a token — proved by the negative.
///
/// Three leak shapes, deliberately not variations of one:
///
/// - `leak-activation-token` — a token beside the URL, caught by the general credential rule;
/// - `leak-activation-fragment` — the implicit-flow `#access_token=…`;
/// - `leak-activation-code` — an authorization *code*, short and unremarkable. No heuristic on the
///   value can see this one. It is refused because an authorize URL is the request half of an OAuth
///   exchange: a `code` on the way out means the deployment handed flux the grant, not the prompt.
///   This is the case that shows the activation rule is doing work the general rule cannot.
#[tokio::test]
async fn an_activation_response_carrying_token_material_is_refused() {
    let fx = Fixture::load("activation").await;

    // The positive control first: a real authorize URL is accepted, so the refusals below are
    // about the token material and not about the path being closed.
    let ok = fx.call("platform.activate", json!({})).await;
    assert!(
        !ok.is_error,
        "the honest activation is refused: {}",
        ok.content
    );
    assert!(
        ok.content
            .contains("https://vendor.example.com/oauth/authorize"),
        "{}",
        ok.content
    );

    for mode in [
        "leak-activation-token",
        "leak-activation-fragment",
        "leak-activation-code",
    ] {
        fx.set_mode(mode);
        let result = fx.call("platform.activate", json!({})).await;
        assert!(
            result.is_error,
            "mode `{mode}`: token material was accepted where an authorize URL was expected: {}",
            result.content
        );
        assert!(
            !result.content.contains(VENDOR_TOKEN) && !result.content.contains("abc123"),
            "mode `{mode}`: the refusal leaked the material: {}",
            result.content
        );
    }
}

// ---------------------------------------------------------------------------
// The journey
// ---------------------------------------------------------------------------

/// activate → refresh → dispatch, end to end, with a hostile deployment at every step: no vendor
/// credential reaches a tool result, the evidence log, or the dispatch record the session log is
/// built from.
///
/// **Scope, stated rather than implied.** The three sinks reachable from this crate are asserted
/// directly: the `ToolResult` (both faces), the shared `EvidenceLog` the executor writes every
/// `tool_call` / `tool.started` / `tool.ended` observation into, and — through the executor — the
/// redacted result the caller receives. The *provider* session log is assembled a layer out, in
/// `flux-agent`, from exactly these `ToolResult`s; `flux-plugin` (L4) does not depend on it and
/// pinning the result is what pins the log entry. Nothing here claims to have observed a provider
/// history.
#[tokio::test]
async fn no_vendor_credential_survives_an_activate_refresh_dispatch_journey() {
    let mut fx = Fixture::load("journey").await;

    let mut registry = ToolRegistry::new();
    for tool in &fx.loaded.tools {
        registry
            .try_register_from("plugin:platform", tool.clone())
            .expect("load-time catalog registers");
    }
    let mut perms = PermissionManager::new();
    for name in ["platform.activate", "platform.dispatch", "platform.echo"] {
        perms.add_allow(name);
    }
    perms.add_allow("platform.zendesk.ticket.create");

    // 1. ACTIVATE — the deployment hands back a grant instead of a prompt.
    fx.set_mode("leak-activation-token");
    let executor = Executor::new(
        registry.clone(),
        perms.clone(),
        Arc::new(AllowApprover),
        fx.ctx.clone(),
    );
    let activated = executor.dispatch("platform.activate", json!({})).await;
    assert!(activated.is_error, "the leaking activation was accepted");

    // 2. REFRESH — the operator authenticated a vendor inside the deployment, so a new op appears.
    fx.set_mode("grown");
    fx.loaded
        .refresh_into(&mut registry, "plugin:platform")
        .await
        .expect("an honest catalog growth refreshes");
    assert!(
        registry
            .names()
            .iter()
            .any(|n| n == "platform.zendesk.ticket.create"),
        "the vendor op did not appear: {:?}",
        registry.names()
    );

    // 3. DISPATCH — the newly-available vendor op leaks the credential in its response.
    fx.set_mode("grown-leak");
    let executor = Executor::new(registry, perms, Arc::new(AllowApprover), fx.ctx.clone());
    let dispatched = executor
        .dispatch("platform.zendesk.ticket.create", json!({ "text": "help" }))
        .await;
    assert!(
        dispatched.is_error,
        "the leaking vendor dispatch was accepted: {}",
        dispatched.content
    );

    // Nothing that flux retained from the journey carries the credential.
    let evidence = serde_json::to_string(&executor.evidence()).expect("evidence serializes");
    for (label, haystack) in [
        ("the activation result", activated.content.as_str()),
        ("the dispatch result", dispatched.content.as_str()),
        ("the evidence log", evidence.as_str()),
    ] {
        for needle in [VENDOR_TOKEN, UNMARKED_VENDOR_SECRET, DSN_PASSWORD] {
            assert!(
                !haystack.contains(needle),
                "a vendor credential reached {label}: {haystack}"
            );
        }
    }
    for view in [activated.view.as_deref(), dispatched.view.as_deref()]
        .into_iter()
        .flatten()
    {
        assert!(
            !view.contains(VENDOR_TOKEN),
            "a credential reached a result view"
        );
    }
}
