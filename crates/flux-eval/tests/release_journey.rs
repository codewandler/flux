//! The offline journey for `examples/release.flux` (C-251) — a fixture repo, a stub scribe, and no
//! network.
//!
//! This is the story's headline proof. Everything the real cut depends on that *can* be exercised
//! offline is exercised here against a throwaway git repo in a temp dir: the host's version
//! derivation, the protocol-line halt, the deterministic changelog insertion, and — the point of the
//! whole design — that **a model asking for a different bump does not change the number**.
//!
//! What is a fixture and why:
//! - **The model** is a stub `task` op returning canned JSON. The program's contract with the scribe
//!   is "text in, text out"; a provider would add nothing but flakiness and a key.
//! - **`scripts/check-crate-versions.sh` and `scripts/cut-release.sh`** are stubs *in the fixture
//!   repo*. The real scripts commit, tag, and run a full cargo gate against the real tree — not
//!   something a test may do. They carry their own `--self-test` modes in CI (`ci.yml`); what this
//!   file owns is the **program's** orchestration: the ordering, the halts, and what does *not*
//!   happen when a step fails.
//!
//! The flow itself is NOT modified for the test: the same `examples/release.flux` that ships is
//! lowered against the live registry and executed here. Only the workspace and the two outer
//! effects (model, scripts) are fixtures.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use flux_core::Result;
use flux_flow::AgentSink;
use flux_runtime::{
    AllowApprover, AuthorityRequirement, Executor, PermissionManager, Tool, ToolContext,
    ToolRegistry, ToolResult,
};
use flux_spec::{AccessKind, Effect, Idempotency, Risk, ToolSpec};
use flux_system::{System, Workspace};
use serde_json::{json, Value};

/// A no-op sink (every `AgentSink` method has a default).
#[derive(Default)]
struct NullSink;
impl AgentSink for NullSink {}

/// The stub scribe: stands in for the `task` op, returning a canned JSON reply. The reply is the
/// only model-shaped input the program takes, which is exactly why the program treats it as prose
/// and never as a decision.
struct StubScribe {
    reply: String,
}

#[async_trait]
impl Tool for StubScribe {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "task".into(),
            description: "Stub sub-agent for the release journey test.".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "role": {"type": "string"}, "task": {"type": "string"} },
                "required": ["role", "task"],
            }),
            output_schema: None,
            effects: vec![Effect::Process],
            risk: Risk::Medium,
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::Provider],
            group: None,
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        params
            .get("role")
            .and_then(|v| v.as_str())
            .map(|r| vec![r.to_string()])
            .unwrap_or_default()
    }

    /// Mirrors the real `TaskTool`: the authority family is provider invocation, not OS process.
    fn authority_requirements(
        &self,
        _params: &Value,
        subjects: &[String],
    ) -> Result<Vec<AuthorityRequirement>> {
        let role = subjects.first().map(String::as_str).unwrap_or("sub-agent");
        Ok(vec![AuthorityRequirement::provider_invoke(role)])
    }

    async fn execute(&self, _ctx: &ToolContext, _params: Value) -> Result<ToolResult> {
        Ok(ToolResult::ok(self.reply.clone()))
    }
}

/// What the stub scribe returns. `bump_opinion` is deliberately settable: the whole load-bearing
/// design decision is that it changes nothing.
fn scribe_reply(bump_opinion: &str) -> String {
    serde_json::to_string(&json!({
        "changelog": "### Fixed\n- Restored the widget cache invalidation (`crates/widget/src/cache.rs:88`).",
        "whats_new": "### Fixed\n- Widget lookups no longer serve stale results after an edit.",
        "bump_opinion": bump_opinion,
        "bump_reason": "the scribe's reasoning, which is advisory only",
    }))
    .expect("canned reply serializes")
}

/// Run `argv` in `dir`, panicking with both streams on failure. Test-setup only — the ops under
/// test go through `flux_system`.
fn sh(dir: &Path, argv: &[&str]) -> String {
    let out = std::process::Command::new(argv[0])
        .args(&argv[1..])
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap_or_else(|e| panic!("spawn {argv:?}: {e}"));
    assert!(
        out.status.success(),
        "{argv:?} failed ({}):\nstdout: {}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// How the fixture's stub `scripts/cut-release.sh` should behave.
enum CutStub {
    /// Roll `[Unreleased]`, commit, and create the annotated tag — the real script's happy path,
    /// printing the same `== cut v<version>.` line the host parses.
    Succeeds,
    /// Exit non-zero *after* restoring the files it touched — the real script's transactional
    /// failure path (C-147): a red gate leaves no phantom version section and no tag.
    FailsTransactionally,
}

/// A throwaway git repo shaped like flux enough for `release.flux` to run against it: a workspace
/// version, both changelogs with an `[Unreleased]` anchor, the two scripts the program is allowed to
/// run, and a commit log with `subjects` after the tag `v0.37.0`.
struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(name: &str, subjects: &[&str], versions_ok: bool, cut: CutStub) -> Self {
        let root = std::env::temp_dir().join(format!(
            "flux-release-journey-{name}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(root.join("scripts")).unwrap();
        std::fs::create_dir_all(root.join("website/docs")).unwrap();

        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace.package]\nversion = \"0.37.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(
            root.join("CHANGELOG.md"),
            "# Changelog\n\n## [Unreleased]\n\n## [0.37.0] - 2026-07-30\n\n### Added\n- The previous release.\n",
        )
        .unwrap();
        std::fs::write(
            root.join("WHATS-NEW.md"),
            "# What's new in flux\n\n<!-- voice rules -->\n\n## [Unreleased]\n\n## [0.37.0] - 2026-07-30\n\n### Added\n- The previous release.\n",
        )
        .unwrap();
        std::fs::write(root.join("website/docs/whats-new.md"), "# What's new\n").unwrap();

        // `scripts/check-crate-versions.sh` — the protocol-line guard. Its failure output names the
        // offending crate, exactly as the real script's `fail()` does.
        let versions = if versions_ok {
            "#!/bin/sh\necho '== crate versions vs v0.37.0 =='\nexit 0\n".to_string()
        } else {
            "#!/bin/sh\necho 'FAIL codewandler-flux-spec changed since v0.37.0 but is still 1.4.0' >&2\nexit 1\n".to_string()
        };
        Self::write_script(&root, "scripts/check-crate-versions.sh", &versions);

        // `scripts/cut-release.sh` — stands in for the real transactional cut.
        let cut_body = match cut {
            CutStub::Succeeds => {
                "#!/bin/sh\nset -e\nNEW=0.37.1\n\
                 [ \"$1\" = minor ] && NEW=0.38.0\n\
                 sed -i \"s/## \\[Unreleased\\]/## [Unreleased]\\n\\n## [$NEW] - 2026-07-31/\" CHANGELOG.md\n\
                 sed -i \"s/## \\[Unreleased\\]/## [Unreleased]\\n\\n## [$NEW] - 2026-07-31/\" WHATS-NEW.md\n\
                 sed -i \"s/0.37.0/$NEW/\" Cargo.toml\n\
                 git add -A\n\
                 git commit -q -m \"chore(release): cut $NEW\"\n\
                 git tag -a \"v$NEW\" -m \"flux $NEW\"\n\
                 echo \"== cut v$NEW. Review 'git show', then prepare + promote the exact commit: ==\"\n"
                    .to_string()
            }
            CutStub::FailsTransactionally => {
                // Mutate, then fail, then restore — the shape the real script's EXIT trap gives.
                "#!/bin/sh\n\
                 cp CHANGELOG.md .snap-changelog\n\
                 sed -i \"s/## \\[Unreleased\\]/## [Unreleased]\\n\\n## [0.37.1] - 2026-07-31/\" CHANGELOG.md\n\
                 echo '!! gate step failed: cargo test --workspace' >&2\n\
                 cp .snap-changelog CHANGELOG.md\n\
                 rm -f .snap-changelog\n\
                 echo '!! restored. Fix the failure and re-run — no phantom version section was left behind.' >&2\n\
                 exit 1\n"
                    .to_string()
            }
        };
        Self::write_script(&root, "scripts/cut-release.sh", &cut_body);

        sh(&root, &["git", "init", "-q", "-b", "main"]);
        sh(&root, &["git", "config", "user.email", "t@example.com"]);
        sh(&root, &["git", "config", "user.name", "Test"]);
        sh(&root, &["git", "add", "-A"]);
        sh(&root, &["git", "commit", "-q", "-m", "chore: seed"]);
        sh(&root, &["git", "tag", "-a", "v0.37.0", "-m", "flux 0.37.0"]);
        for subject in subjects {
            sh(
                &root,
                &["git", "commit", "-q", "--allow-empty", "-m", subject],
            );
        }
        Fixture { root }
    }

    fn write_script(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        std::fs::write(&path, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.root.join(rel)).unwrap_or_default()
    }

    /// Every tag in the fixture repo. The release contract is "no tag unless the cut succeeded", so
    /// this is the assertion that matters most.
    fn tags(&self) -> Vec<String> {
        sh(&self.root, &["git", "tag", "--list"])
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect()
    }

    fn head_subject(&self) -> String {
        sh(&self.root, &["git", "log", "-1", "--format=%s"])
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).ok();
    }
}

/// The outcome of running `examples/release.flux` against a fixture.
struct RunOutcome {
    /// The flow's returned string, when it ran to completion.
    result: Option<String>,
    /// The error/halt text, when it did not.
    failure: Option<String>,
}

impl RunOutcome {
    fn expect_ok(&self) -> &str {
        self.result
            .as_deref()
            .unwrap_or_else(|| panic!("flow did not complete: {:?}", self.failure))
    }

    fn expect_halt(&self) -> &str {
        self.failure
            .as_deref()
            .unwrap_or_else(|| panic!("flow completed but should have halted: {:?}", self.result))
    }
}

/// Lower and run the shipped `examples/release.flux` against `fixture`, with `apply` bound as the
/// flow input and `scribe_reply` as the stub model's textual response.
async fn run_release_flow_with_reply(
    fixture: &Fixture,
    apply: bool,
    scribe_reply: String,
) -> RunOutcome {
    let src = std::fs::read_to_string(release_flux_path()).expect("read examples/release.flux");
    let mut ast = match flux_flow::program::Module::parse_str(&src)
        .expect("examples/release.flux parses as native flux-lang text")
    {
        flux_flow::program::Module::Flow(ast) => ast,
        flux_flow::program::Module::Program(p) => p
            .flows
            .first()
            .cloned()
            .expect("release.flux declares a flow"),
    };

    let mut registry = ToolRegistry::new();
    flux_tools::register_builtins(&mut registry);
    flux_eval::register_eval_ops(&mut registry);
    registry.register(Arc::new(StubScribe {
        reply: scribe_reply,
    }));

    // The same gate `flux flow run` applies — unknown ops, missing required params, type conflicts.
    let ops = flux_flow::registry::OpRegistry::new(&registry);
    flux_flow::analyze::lower(&ast, &ops, &Default::default())
        .unwrap_or_else(|diags| panic!("examples/release.flux fails the flow-run gate: {diags:?}"));

    // Bind the declared flow input exactly the way `flux flow run --arg` does (a `Lit` prefix bind).
    bind_input(&mut ast, "apply", json!(apply));

    let executor = Executor::new(
        registry,
        PermissionManager::from_rules(&["*".into()], &[]),
        Arc::new(AllowApprover),
        ToolContext::new(Arc::new(System::new(
            Workspace::new(&fixture.root).unwrap(),
        ))),
    );
    let store = flux_flow::state::FlowStore::in_memory().unwrap();
    let mut sink = NullSink;
    match flux_flow::runtime::execute_flow(&store, &executor, "release-journey", &ast, &mut sink)
        .await
    {
        Ok(outcome) => RunOutcome {
            result: Some(outcome.result),
            failure: None,
        },
        Err(e) => RunOutcome {
            result: None,
            failure: Some(e.to_string()),
        },
    }
}

/// Normal journey helper: vary only the scribe's advisory opinion while keeping its JSON valid.
async fn run_release_flow(fixture: &Fixture, apply: bool, scribe_opinion: &str) -> RunOutcome {
    run_release_flow_with_reply(fixture, apply, scribe_reply(scribe_opinion)).await
}

fn release_flux_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/release.flux")
}

/// Prepend `name = <value>` as a literal bind, mirroring `prepare_cli_flow_inputs`.
fn bind_input(ast: &mut flux_flow::ast::DraftAst, name: &str, value: Value) {
    let mut prefix = vec![flux_flow::ast::Node::Bind {
        name: name.into(),
        value: Box::new(flux_flow::ast::Node::Lit { value }),
        ty: None,
        effect: None,
    }];
    prefix.append(&mut ast.body);
    ast.body = prefix;
}

// ---------------------------------------------------------------------------
// The journey
// ---------------------------------------------------------------------------

/// The headline case: a dry run derives the version, renders both changelog sections, and mutates
/// **nothing** — no commit, no tag, not even the changelog it is about to edit.
#[tokio::test]
async fn a_dry_run_derives_the_version_and_renders_both_sections_without_touching_history() {
    let fixture = Fixture::new(
        "dry",
        &[
            "fix(widget): restore cache invalidation",
            "docs(readme): fix a broken link",
        ],
        true,
        CutStub::Succeeds,
    );
    let before_changelog = fixture.read("CHANGELOG.md");
    let before_whats_new = fixture.read("WHATS-NEW.md");

    let outcome = run_release_flow(&fixture, false, "patch").await;
    let summary = outcome.expect_ok();

    // The host's version, derived from `fix:`/`docs:` titles only.
    assert!(
        summary.contains("0.37.1"),
        "dry run must report the derived version, got: {summary}"
    );
    // Both audiences' prose reached the preview.
    assert!(
        summary.contains("Restored the widget cache invalidation"),
        "dry run must render the engineer-facing section, got: {summary}"
    );
    assert!(
        summary.contains("Widget lookups no longer serve stale results"),
        "dry run must render the customer-facing section, got: {summary}"
    );

    // And nothing moved.
    assert_eq!(
        fixture.read("CHANGELOG.md"),
        before_changelog,
        "a dry run must not write CHANGELOG.md"
    );
    assert_eq!(
        fixture.read("WHATS-NEW.md"),
        before_whats_new,
        "a dry run must not write WHATS-NEW.md"
    );
    assert_eq!(
        fixture.tags(),
        vec!["v0.37.0"],
        "a dry run must create no tag"
    );
    assert_eq!(
        fixture.head_subject(),
        "docs(readme): fix a broken link",
        "a dry run must create no commit"
    );
}

/// The load-bearing decision, pinned: the scribe argues for `minor` on a log that is patch-only, and
/// the host cuts `patch` anyway — loudly.
#[tokio::test]
async fn a_scribe_asking_for_a_different_bump_does_not_change_the_number() {
    let fixture = Fixture::new(
        "disagree",
        &["fix(widget): restore cache invalidation"],
        true,
        CutStub::Succeeds,
    );

    let outcome = run_release_flow(&fixture, false, "minor").await;
    let summary = outcome.expect_ok();

    assert!(
        summary.contains("0.37.1"),
        "the host's patch bump must stand against a `minor` opinion, got: {summary}"
    );
    assert!(
        !summary.contains("0.38.0"),
        "the scribe's opinion must not reach the version, got: {summary}"
    );
    assert!(
        summary.to_lowercase().contains("warning"),
        "a bump disagreement must surface loudly, got: {summary}"
    );
}

/// A conventional-commit `!` is the repo's mechanical breaking signal, and while `0.y` that means
/// **minor**.
#[tokio::test]
async fn a_breaking_title_derives_a_minor_bump() {
    let fixture = Fixture::new(
        "breaking",
        &[
            "fix(widget): restore cache invalidation",
            "refactor(events,sdk,cli)!: collapse the emitter seam",
        ],
        true,
        CutStub::Succeeds,
    );

    let outcome = run_release_flow(&fixture, false, "minor").await;
    let summary = outcome.expect_ok();
    assert!(
        summary.contains("0.38.0"),
        "a `!` title must derive minor while 0.y, got: {summary}"
    );
}

/// An unbumped protocol-line crate halts the run **before any release artifact exists**, naming the
/// crate. A model must never reason about wire compatibility, so this is a stop, not a guess.
#[tokio::test]
async fn an_unbumped_protocol_line_crate_halts_before_anything_is_written() {
    let fixture = Fixture::new(
        "protocol",
        &["fix(widget): restore cache invalidation"],
        false,
        CutStub::Succeeds,
    );
    let before_changelog = fixture.read("CHANGELOG.md");

    // `apply = true`: this must halt on its own merit, not because it was a dry run.
    let outcome = run_release_flow(&fixture, true, "patch").await;
    let failure = outcome.expect_halt();

    assert!(
        failure.contains("codewandler-flux-spec"),
        "the halt must name the protocol-line crate, got: {failure}"
    );
    assert_eq!(
        fixture.read("CHANGELOG.md"),
        before_changelog,
        "the halt must precede any changelog mutation"
    );
    assert_eq!(
        fixture.tags(),
        vec!["v0.37.0"],
        "the halt must leave no tag"
    );
}

/// `task()` returns text. Every model wrapper or schema drift must halt at the explicit host parser,
/// before that text can reach either changelog or the cut script.
#[tokio::test]
async fn malformed_scribe_text_halts_before_any_changelog_or_tag() {
    let valid = scribe_reply("patch");
    let replies = [
        format!("```json\n{valid}\n```"),
        format!("Here are the notes: {valid}"),
        format!("{valid}\nHope this helps."),
        r#"{"changelog":"c","whats_new":"w","bump_opinion":"major","bump_reason":"r"}"#.into(),
        r#"{"changelog":"c","whats_new":"w","bump_opinion":"patch"}"#.into(),
    ];

    for (index, reply) in replies.into_iter().enumerate() {
        let fixture = Fixture::new(
            &format!("malformed-{index}"),
            &["fix(widget): restore cache invalidation"],
            true,
            CutStub::Succeeds,
        );
        let before_changelog = fixture.read("CHANGELOG.md");
        let before_whats_new = fixture.read("WHATS-NEW.md");
        let before_manifest = fixture.read("Cargo.toml");
        let before_website = fixture.read("website/docs/whats-new.md");
        let before_head = fixture.head_subject();

        let failure = run_release_flow_with_reply(&fixture, true, reply)
            .await
            .expect_halt()
            .to_string();
        assert!(
            failure.contains("release_parse_notes"),
            "halt must name the model boundary, got: {failure}"
        );
        assert_eq!(fixture.read("CHANGELOG.md"), before_changelog);
        assert_eq!(fixture.read("WHATS-NEW.md"), before_whats_new);
        assert_eq!(fixture.read("Cargo.toml"), before_manifest);
        assert_eq!(fixture.read("website/docs/whats-new.md"), before_website);
        assert_eq!(fixture.head_subject(), before_head);
        assert_eq!(fixture.tags(), vec!["v0.37.0"]);
    }
}

/// A red gate inside `cut-release.sh` leaves **no tag** and no phantom version section — the C-147
/// property, which the program must not lose by wrapping the script.
#[tokio::test]
async fn a_red_gate_in_the_cut_leaves_no_tag_and_no_phantom_version_section() {
    let fixture = Fixture::new(
        "redgate",
        &["fix(widget): restore cache invalidation"],
        true,
        CutStub::FailsTransactionally,
    );

    let outcome = run_release_flow(&fixture, true, "patch").await;
    outcome.expect_halt();

    assert_eq!(
        fixture.tags(),
        vec!["v0.37.0"],
        "a red gate must leave no tag"
    );
    let changelog = fixture.read("CHANGELOG.md");
    assert!(
        !changelog.contains("## [0.37.1]"),
        "a failed cut must leave no phantom version section:\n{changelog}"
    );
}

/// The apply path, end to end: the prose lands under `[Unreleased]` (inserted by the host, not
/// written by the model), the cut runs, and the annotated tag exists.
#[tokio::test]
async fn an_applied_run_inserts_the_prose_and_produces_exactly_one_new_tag() {
    let fixture = Fixture::new(
        "apply",
        &["fix(widget): restore cache invalidation"],
        true,
        CutStub::Succeeds,
    );

    let outcome = run_release_flow(&fixture, true, "patch").await;
    let summary = outcome.expect_ok();
    assert!(summary.contains("0.37.1"), "got: {summary}");

    let changelog = fixture.read("CHANGELOG.md");
    assert!(
        changelog.contains("Restored the widget cache invalidation"),
        "the engineer-facing prose must be inserted:\n{changelog}"
    );
    let whats_new = fixture.read("WHATS-NEW.md");
    assert!(
        whats_new.contains("Widget lookups no longer serve stale results"),
        "the customer-facing prose must be inserted:\n{whats_new}"
    );
    assert!(
        !whats_new.contains("crates/widget/src/cache.rs"),
        "the customer changelog must not carry engineer-facing detail:\n{whats_new}"
    );

    let mut tags = fixture.tags();
    tags.sort();
    assert_eq!(
        tags,
        vec!["v0.37.0", "v0.37.1"],
        "the applied run must produce exactly one new tag"
    );
}

/// Re-running on an already-released SHA is a no-op: with no commits since the tag there is nothing
/// to cut, and the flow says so instead of minting an empty release.
#[tokio::test]
async fn a_second_run_on_an_already_released_sha_is_a_no_op() {
    let fixture = Fixture::new("idempotent", &[], true, CutStub::Succeeds);

    let outcome = run_release_flow(&fixture, true, "patch").await;
    let summary = outcome.expect_ok();
    assert!(
        summary.contains("nothing to cut"),
        "an already-released SHA must be a no-op, got: {summary}"
    );
    assert_eq!(
        fixture.tags(),
        vec!["v0.37.0"],
        "a no-op run must create no tag"
    );
}
