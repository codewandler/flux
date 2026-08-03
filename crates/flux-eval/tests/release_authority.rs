//! The authority envelope around `examples/release.flux` (C-251).
//!
//! An unattended agent with write authority in a release pipeline is the highest-risk shape in this
//! repo, and crates.io is yank-only. So the release flow's authority is narrow **by construction**,
//! and this file pins each half of that claim against the shipped artifacts rather than against a
//! prompt:
//!
//! 1. **The op set is the ceiling.** The program is parsed and every operation it calls is checked
//!    against an allow-list. It holds no `bash`, no `proc.run`, no `write`/`edit`/`patch`/`append` —
//!    so there is no general process or write authority to scope in the first place.
//! 2. **Process authority is fixed argv.** The two process-capable release ops name their script as
//!    their permission subject and take no program from a caller.
//! 3. **Write authority is three files.** `changelog_insert` resolves its target through
//!    `flux-system`'s canonicalizing IO boundary and refuses anything else — including via `.`, `..`,
//!    and an absolute spelling of an out-of-scope path.
//! 4. **The scribe has no tools at all.** `tools: []` in the role file, so the model that drafts the
//!    prose has no write op to attempt in the first place.
//!
//! What this file does NOT claim: that a `flux flow run` today installs a *path-scoped policy floor*.
//! It cannot — `flux-cli` composes `[[policy.grants]]` **additively** on top of
//! `flux_policy::default_local_grants()`, which already grants `workspace.write` on `path: "*"`
//! (`crates/flux-cli/src/execution.rs:1419`). `.flux/policies/release.toml` is the narrow policy
//! written out and verified here for the day a surface can install it in place of the floor; the
//! enforcement the release flow relies on **today** is (1)–(4) above, which no policy composition can
//! widen. See the story's Progress note.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use flux_eval::release::{RELEASE_SCRIPTS, WRITABLE_CHANGELOGS};
use flux_runtime::{Tool, ToolContext};
use flux_system::{System, Workspace};
use serde_json::{json, Value};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn release_flow_workflow_code() -> String {
    format!(
        "{}\n{}",
        workflow_code("release-flow.yml"),
        non_comment_source(repo_root().join("scripts/promote-release-flow.sh"))
    )
}

fn release_workflow_code() -> String {
    workflow_code("release.yml")
}

fn workflow_code(name: &str) -> String {
    non_comment_source(repo_root().join(".github/workflows").join(name))
}

fn non_comment_source(path: PathBuf) -> String {
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

fn code_line_index<F>(code: &str, predicate: F) -> Option<usize>
where
    F: Fn(&str) -> bool,
{
    code.lines().position(predicate)
}

fn code_line_indices<F>(code: &str, predicate: F) -> Vec<usize>
where
    F: Fn(&str) -> bool,
{
    code.lines()
        .enumerate()
        .filter_map(|(index, line)| predicate(line).then_some(index))
        .collect()
}

fn is_git_push_line(line: &str) -> bool {
    let line = line.trim_start();
    line.starts_with("git push") || line.starts_with("git_with_release_token push")
}

/// Every operation `examples/release.flux` calls. Collected from the serialized AST rather than a
/// hand-written match, so a new node kind cannot hide a call from this check.
fn ops_called_by_release_flux() -> BTreeSet<String> {
    let src = std::fs::read_to_string(repo_root().join("examples/release.flux"))
        .expect("read examples/release.flux");
    let ast = match flux_flow::program::Module::parse_str(&src).expect("release.flux parses") {
        flux_flow::program::Module::Flow(ast) => ast,
        flux_flow::program::Module::Program(p) => {
            p.flows.first().cloned().expect("release.flux has a flow")
        }
    };
    let mut found = BTreeSet::new();
    collect_ops(
        &serde_json::to_value(&ast).expect("AST serializes"),
        &mut found,
    );
    found
}

fn collect_ops(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(op)) = map.get("op") {
                out.insert(op.clone());
            }
            for v in map.values() {
                collect_ops(v, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|v| collect_ops(v, out)),
        _ => {}
    }
}

/// The ceiling. Widening this list widens what an unattended release run can do, so it is spelled out
/// with a reason per entry and no wildcards.
const PERMITTED_OPS: &[(&str, &str)] = &[
    ("release_plan", "reads git + Cargo.toml; derives the bump"),
    (
        "release_verify_versions",
        "fixed argv: scripts/check-crate-versions.sh",
    ),
    (
        "release_parse_notes",
        "pure strict JSON adaptation; grants no external authority",
    ),
    ("release_cut", "fixed argv: scripts/cut-release.sh"),
    (
        "changelog_insert",
        "write authority scoped to the three release changelogs",
    ),
    ("task", "the scribe sub-agent; returns text, holds no tools"),
    (
        "observe",
        "records the bump disagreement for the audit trail",
    ),
    ("fmt", "pure string interpolation"),
    ("jq", "pure field access sugar (`$plan.bump`)"),
    (
        "expr",
        "pure comparison (`$count == 0`, `$opinion != $bump`)",
    ),
];

#[test]
fn the_release_program_calls_only_its_permitted_ops() {
    let called = ops_called_by_release_flux();
    let permitted: BTreeSet<&str> = PERMITTED_OPS.iter().map(|(name, _)| *name).collect();
    let extra: Vec<&String> = called
        .iter()
        .filter(|op| !permitted.contains(op.as_str()))
        .collect();
    assert!(
        extra.is_empty(),
        "examples/release.flux calls op(s) outside its authority ceiling: {extra:?}\n\
         Adding an op to a release flow widens what an unattended run with commit and tag authority \
         can do. If the addition is intended, add it to PERMITTED_OPS with the reason it is safe."
    );
}

#[test]
fn the_release_program_validates_scribe_text_before_field_access() {
    let called = ops_called_by_release_flux();
    assert!(
        called.contains("release_parse_notes"),
        "the task op returns text; release.flux must pass it through the strict host parser before \
         reading changelog fields"
    );
}

/// The ops that would hand the release flow general authority. Named individually so the failure
/// message says which one appeared.
#[test]
fn the_release_program_holds_no_general_process_or_write_authority() {
    let called = ops_called_by_release_flux();
    for forbidden in [
        "bash",
        "proc.run",
        "write",
        "edit",
        "patch",
        "append",
        "git_reset",
        "web.fetch",
    ] {
        assert!(
            !called.contains(forbidden),
            "examples/release.flux must never call `{forbidden}` — it would give an unattended \
             release run authority the narrow release ops exist to avoid"
        );
    }
}

/// Process authority is the script, not a program name a caller chose.
#[test]
fn the_release_ops_name_their_script_as_their_permission_subject() {
    assert_eq!(
        flux_eval::release::ReleaseVerifyVersionsTool.permission_subjects(&json!({})),
        vec![RELEASE_SCRIPTS[0].to_string()],
    );
    assert_eq!(
        flux_eval::release::ReleaseCutTool.permission_subjects(&json!({ "bump": "patch" })),
        vec![RELEASE_SCRIPTS[1].to_string()],
    );
    assert_eq!(
        RELEASE_SCRIPTS,
        &["scripts/check-crate-versions.sh", "scripts/cut-release.sh"],
        "the release flow's entire process authority — pinned so widening it is deliberate"
    );
}

/// A fixture workspace holding the three release changelogs plus a source file to aim at.
struct Workspace0 {
    root: PathBuf,
}

impl Workspace0 {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "flux-release-authority-{name}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("website/docs")).unwrap();
        std::fs::create_dir_all(root.join("crates/flux-core/src")).unwrap();
        for f in WRITABLE_CHANGELOGS {
            std::fs::write(root.join(f), "# Doc\n\n## [Unreleased]\n\n## [0.37.0]\n").unwrap();
        }
        std::fs::write(root.join("crates/flux-core/src/lib.rs"), "// source\n").unwrap();
        std::fs::write(root.join("README.md"), "# readme\n\n## [Unreleased]\n").unwrap();
        Workspace0 { root }
    }

    fn ctx(&self) -> ToolContext {
        ToolContext::new(Arc::new(System::new(
            Workspace::new(&self.root).expect("workspace"),
        )))
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.root.join(rel)).unwrap_or_default()
    }
}

impl Drop for Workspace0 {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).ok();
    }
}

async fn insert(ws: &Workspace0, file: &str, apply: bool) -> flux_runtime::ToolResult {
    flux_eval::release::ChangelogInsertTool
        .execute(
            &ws.ctx(),
            json!({ "file": file, "body": "### Fixed\n- a thing", "apply": apply }),
        )
        .await
        .expect("op runs")
}

/// The model's prose can only ever land in the three release changelogs. This is the "attempt a write
/// elsewhere" case: the target is refused after normalization, before any read or write happens, and
/// no approver or permission rule is involved — so `--yes` cannot turn it into an allow.
#[tokio::test]
async fn a_write_outside_the_three_changelogs_is_refused() {
    let ws = Workspace0::new("outside");

    for target in [
        // Plain source file.
        "crates/flux-core/src/lib.rs",
        // A file that *has* an `[Unreleased]` anchor, so only the path scope can refuse it.
        "README.md",
        // The same source file reached through a traversal and a `.` — normalization must collapse
        // these before the comparison, not after.
        "./crates/flux-core/src/lib.rs",
        "website/docs/../../crates/flux-core/src/lib.rs",
        "website/../CHANGELOG.md/../crates/flux-core/src/lib.rs",
    ] {
        let result = insert(&ws, target, true).await;
        assert!(
            result.is_error,
            "changelog_insert must refuse `{target}`, got: {}",
            result.content
        );
        assert!(
            result.content.contains("refuses"),
            "the refusal must say so plainly for `{target}`, got: {}",
            result.content
        );
    }

    assert_eq!(
        ws.read("crates/flux-core/src/lib.rs"),
        "// source\n",
        "no refused attempt may have written anything"
    );
    assert_eq!(ws.read("README.md"), "# readme\n\n## [Unreleased]\n");
}

/// …and the three that are in scope work, including through a non-canonical spelling.
#[tokio::test]
async fn the_three_release_changelogs_are_writable() {
    let ws = Workspace0::new("inside");
    for target in [
        "CHANGELOG.md",
        "./WHATS-NEW.md",
        "website/docs/whats-new.md",
    ] {
        let result = insert(&ws, target, true).await;
        assert!(
            !result.is_error,
            "changelog_insert must accept `{target}`, got: {}",
            result.content
        );
    }
    for f in WRITABLE_CHANGELOGS {
        assert!(
            ws.read(f).contains("### Fixed"),
            "{f} should carry the inserted prose"
        );
    }
}

/// A dry run reads and diffs but never writes — the property the whole `workflow_dispatch` default
/// rests on.
#[tokio::test]
async fn a_preview_writes_nothing() {
    let ws = Workspace0::new("preview");
    let before = ws.read("CHANGELOG.md");
    let result = insert(&ws, "CHANGELOG.md", false).await;
    assert!(!result.is_error, "{}", result.content);
    assert!(
        result.content.contains("preview"),
        "a preview must say so: {}",
        result.content
    );
    assert_eq!(ws.read("CHANGELOG.md"), before, "a preview must not write");
}

/// The scribe holds no tools. `tools: []` is what makes "the model cannot write the file"
/// structural — the sentence in the role's prose is documentation of that fact, not the mechanism.
#[test]
fn the_release_scribe_role_declares_no_tools() {
    let role = std::fs::read_to_string(repo_root().join(".flux/agents/release-scribe.md"))
        .expect("read .flux/agents/release-scribe.md");
    let frontmatter = role
        .strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---"))
        .map(|(fm, _)| fm)
        .expect("the role has YAML frontmatter");
    assert!(
        frontmatter.lines().any(|l| l.trim() == "tools: []"),
        "release-scribe must declare `tools: []` — an empty toolset is what stops the model that \
         drafts release notes from also writing them. Frontmatter was:\n{frontmatter}"
    );
}

/// The checked-in narrow policy says what it claims to say. This does not install it (see the module
/// doc); it verifies the artifact so the follow-up that wires it cannot inherit a typo — and so the
/// three writable paths cannot drift from `WRITABLE_CHANGELOGS`.
#[test]
fn the_checked_in_release_policy_grants_exactly_the_three_changelogs_and_two_scripts() {
    let raw = std::fs::read_to_string(repo_root().join(".flux/policies/release.toml"))
        .expect("read .flux/policies/release.toml");
    let doc: toml::Value = toml::from_str(&raw).expect("release.toml parses as TOML");
    let grants = doc
        .get("policy")
        .and_then(|p| p.get("grants"))
        .and_then(|g| g.as_array())
        .expect("[[policy.grants]] present");

    let mut write_paths = BTreeSet::new();
    let mut exec_ids = BTreeSet::new();
    let mut all_matchers: Vec<String> = Vec::new();
    for grant in grants {
        let actions: Vec<&str> = grant
            .get("actions")
            .and_then(|a| a.as_array())
            .expect("grant has actions")
            .iter()
            .filter_map(|a| a.as_str())
            .collect();
        for resource in grant
            .get("resources")
            .and_then(|r| r.as_array())
            .expect("grant has resources")
        {
            for key in ["path", "id", "name"] {
                if let Some(v) = resource.get(key).and_then(|v| v.as_str()) {
                    all_matchers.push(v.to_string());
                }
            }
            if actions.contains(&"workspace.write") {
                if let Some(p) = resource.get("path").and_then(|p| p.as_str()) {
                    write_paths.insert(p.to_string());
                }
            }
            if actions.contains(&"process.exec") {
                if let Some(id) = resource.get("id").and_then(|i| i.as_str()) {
                    exec_ids.insert(id.to_string());
                }
            }
        }
    }

    assert_eq!(
        write_paths,
        WRITABLE_CHANGELOGS
            .iter()
            .map(|s| s.to_string())
            .collect::<BTreeSet<_>>(),
        "the policy's write paths must be exactly WRITABLE_CHANGELOGS — no globs, no extras"
    );
    // `git` earns its grant because reading the commit log IS the host's version evidence, and the
    // ops only run read-only subcommands; the mutating half lives inside `cut-release.sh`.
    let expected_exec: BTreeSet<String> = RELEASE_SCRIPTS
        .iter()
        .map(|s| s.to_string())
        .chain(std::iter::once("git".to_string()))
        .collect();
    assert_eq!(
        exec_ids, expected_exec,
        "the policy's exec grants must be exactly `git` plus the two release scripts"
    );
    // `flux_policy`'s `*` matches any run of characters *including* `/`, so a single wildcard
    // anywhere in a grant would silently widen this policy far past what its comments claim.
    let wild: Vec<&String> = all_matchers.iter().filter(|m| m.contains('*')).collect();
    assert!(
        wild.is_empty(),
        "every resource matcher in this policy must be literal; found wildcard(s): {wild:?}"
    );
}

// ---------------------------------------------------------------------------
// The unattended half
// ---------------------------------------------------------------------------

/// C-251 is not complete when the safe offline program merely exists: merging `main` into the
/// dedicated `release` branch must be the deliberate action that runs it in apply mode. Ordinary
/// pushes to `main` must not cut, and a manual preview may remain as a separate trigger.
#[test]
fn the_release_branch_is_the_automatic_apply_trigger() {
    let code = release_flow_workflow_code();
    let release_push = code.contains("  push:\n    branches:\n      - release")
        || code.contains("  push:\n    branches:\n      - \"release\"")
        || code.contains("  push:\n    branches: [release]")
        || code.contains("  push:\n    branches: [\"release\"]");
    assert!(
        release_push,
        ".github/workflows/release-flow.yml must run on pushes to the dedicated `release` branch; \
         workflow_dispatch alone still leaves the release as a human sequence"
    );
    assert!(
        !code.contains("  push:\n    branches:\n      - main")
            && !code.contains("  push:\n    branches: [main]"),
        "ordinary pushes to `main` must not cut a release; merging main -> release is the deliberate act"
    );

    let push_applies = code.contains("--arg apply=true")
        || (code.contains("--arg apply=")
            && code.contains("github.event_name")
            && code.contains("push"));
    assert!(
        push_applies,
        "the release-branch path must run examples/release.flux with apply=true; a push-triggered \
         preview creates no commit or tag and therefore releases nothing"
    );
}

/// The cut commit is first staged on a versioned candidate ref. `main` does not move until that
/// exact SHA has a successful candidate and a matching receipt; the irreversible tag comes last.
/// This avoids leaving main in a cut-but-unpublishable state when a platform build fails, and keeps
/// the build-once ordering documented by `cut-release.sh`.
#[test]
fn the_release_workflow_prepares_an_exact_sha_candidate_before_pushing_the_tag() {
    let code = release_flow_workflow_code();

    let versioned_candidate_ref = code.contains("release-candidates/v")
        || (code.contains("TAG=\"v$VERSION\"")
            && code.contains("CANDIDATE_BRANCH=\"release-candidates/$TAG\""));
    assert!(
        versioned_candidate_ref,
        "the cut must be staged at refs/heads/release-candidates/v$version before main moves"
    );
    let candidate_ref_push = code_line_index(&code, |line| {
        is_git_push_line(line)
            && (line.contains("release-candidates/v")
                || line.to_ascii_lowercase().contains("candidate_ref"))
    });
    let main_push = code_line_index(&code, |line| {
        is_git_push_line(line)
            && (line.contains("HEAD:main")
                || line.contains("refs/heads/main")
                || line.trim_end().ends_with(" origin main"))
    });
    let candidate_dispatch =
        code_line_index(&code, |line| line.contains("gh workflow run release.yml"));
    let run_watches = code_line_indices(&code, |line| line.contains("gh run watch"));
    let candidate_wait = run_watches.first().copied();
    let release_wait = run_watches
        .last()
        .copied()
        .filter(|_| run_watches.len() >= 2);
    let exact_candidate = code_line_index(&code, |line| {
        line.contains("scripts/find-release-candidate.sh")
            && line.to_ascii_lowercase().contains("sha")
    });
    let receipt_verify = code_line_index(&code, |line| {
        line.contains("scripts/release-candidate.sh verify")
    });
    let tag_push = code_line_index(&code, |line| {
        is_git_push_line(line)
            && line.to_ascii_lowercase().contains("tag")
            && !line.contains("HEAD:main")
            && !line.trim_end().ends_with(" origin main")
    });
    let public_verify = code_line_index(&code, |line| {
        line.contains("scripts/verify-github-release.sh")
            && !line.contains("--staged")
            && line.to_ascii_lowercase().contains("tag")
    });

    let ordered = match (
        candidate_ref_push,
        candidate_dispatch,
        candidate_wait,
        exact_candidate,
        receipt_verify,
        main_push,
        tag_push,
        release_wait,
        public_verify,
    ) {
        (
            Some(candidate_ref),
            Some(dispatch),
            Some(candidate_wait),
            Some(candidate),
            Some(receipt),
            Some(main),
            Some(tag),
            Some(release_wait),
            Some(public_verify),
        ) => {
            candidate_ref < dispatch
                && dispatch < candidate_wait
                && candidate_wait < candidate
                && candidate < receipt
                && receipt < main
                && main < tag
                && tag < release_wait
                && release_wait < public_verify
        }
        _ => false,
    };
    assert!(
        ordered,
        "release-flow.yml must perform, in order: stage the cut at the versioned candidate ref; \
         dispatch and wait for release.yml; select the exact-SHA candidate and verify its receipt; \
         advance main; push the tag; wait for its Release workflow; verify the public Release. \
         Found indexes: candidate_ref={candidate_ref_push:?}, dispatch={candidate_dispatch:?}, \
         candidate_wait={candidate_wait:?}, exact={exact_candidate:?}, receipt={receipt_verify:?}, \
         main={main_push:?}, tag={tag_push:?}, release_wait={release_wait:?}, \
         public_verify={public_verify:?}"
    );
    assert!(
        code.contains("git rev-list -n1") && code.contains("^{}"),
        "the promotion path must resolve the annotated tag to its commit and use that exact SHA as \
         the candidate identity"
    );
}

/// Candidate preparation may run from the one version-derived staging ref and nowhere else. In
/// particular, accepting arbitrary branches would let a same-version dispatch prepare artifacts
/// for a ref that the release flow never reviewed.
#[test]
fn release_yml_accepts_only_the_exact_versioned_candidate_ref() {
    let code = release_workflow_code();
    let exact_ref = code.contains("refs/heads/release-candidates/v$CANDIDATE_VERSION")
        || code.contains("refs/heads/release-candidates/v${CANDIDATE_VERSION}");
    assert!(
        exact_ref,
        "release.yml must derive the only permitted dispatch ref as \
         refs/heads/release-candidates/v$CANDIDATE_VERSION"
    );
    assert!(
        code.lines().any(|line| {
            line.contains("DISPATCH_REF")
                && line.contains("!=")
                && (line.contains("release-candidates/")
                    || line.contains("EXPECTED")
                    || line.contains("expected"))
        }),
        "release.yml must fail when github.ref differs from its version-derived candidate ref"
    );
    assert!(
        !code.lines().any(|line| {
            line.contains("DISPATCH_REF") && line.contains("!=") && line.contains("refs/heads/main")
        }),
        "candidate preparation must no longer accept main directly"
    );
}

/// The release gate must not depend on an OpenRouter account balance. The direct Anthropic key is
/// the stable default, while an explicit OpenRouter model remains supported and must select its own
/// credential rather than silently borrowing the default provider's key.
#[test]
fn release_flow_defaults_to_direct_anthropic_and_selects_the_model_credential() {
    let code = workflow_code("release-flow.yml");
    assert!(
        code.contains("anthropic/claude-haiku-4-5"),
        "release-flow.yml must default to the direct Anthropic Haiku model"
    );
    assert!(
        !code.contains("default: \"openrouter/anthropic/claude-haiku-4.5\"")
            && !code.contains("inputs.model || 'openrouter/anthropic/claude-haiku-4.5'"),
        "the automatic release must not depend on OpenRouter account credits by default"
    );
    for required in [
        "secrets.ANTHROPIC_API_KEY",
        "secrets.OPENROUTER_API_KEY",
        "anthropic/*",
        "openrouter/*",
    ] {
        assert!(
            code.contains(required),
            "release-flow.yml must select provider credentials explicitly; missing `{required}`"
        );
    }
}

/// Unattended agentic and served surfaces fail closed when no OS sandbox backend exists. Hosted
/// Ubuntu runners do not provide bubblewrap by default, so the release workflow must provision and
/// prove it before either the live smoke or the Flux-authored cut runs.
#[test]
fn release_flow_proves_a_sandbox_backend_before_running_flux() {
    let code = workflow_code("release-flow.yml");
    let backend = code_line_index(&code, |line| {
        line.contains("apt-get install") && line.contains("bubblewrap")
    });
    let user_namespace = code_line_index(&code, |line| {
        line.contains("apparmor_restrict_unprivileged_userns=0")
    });
    let backend_probe = code_line_index(&code, |line| {
        line.contains("bwrap") && line.contains("--ro-bind") && line.contains("/ / ")
    });
    let smoke = code_line_index(&code, |line| line.contains("./scripts/smoke-live.sh"));
    let flow = code_line_index(&code, |line| {
        line.contains("flux flow run examples/release.flux")
    });
    assert!(
        matches!((backend, user_namespace, backend_probe, smoke, flow),
            (Some(backend), Some(userns), Some(probe), Some(smoke), Some(flow))
                if backend < userns && userns < probe && probe < smoke && smoke < flow),
        "release-flow.yml must install bubblewrap, enable the hosted Ubuntu user-namespace \
         primitive, and self-test the backend before the live smoke and Flux flow; found \
         install={backend:?}, userns={user_namespace:?}, probe={backend_probe:?}, smoke={smoke:?}, \
         flow={flow:?}"
    );
}

/// GitHub deliberately suppresses workflow runs caused by refs pushed with `GITHUB_TOKEN`. The
/// release and crates.io workflows are tag-push-triggered, so this workflow needs a separately
/// configured push credential; otherwise a green auto-cut silently publishes nothing.
#[test]
fn the_tag_push_uses_a_credential_that_can_trigger_the_publication_workflows() {
    let code = release_flow_workflow_code();
    assert!(
        code.contains("secrets.RELEASE_TOKEN"),
        "release-flow.yml must use a non-GITHUB_TOKEN credential for its main/tag pushes; refs \
         pushed with GITHUB_TOKEN do not trigger release.yml or crates-io.yml"
    );
    assert!(
        code.contains("actions: write") && code.contains("contents: read"),
        "the workflow token needs Actions write to dispatch/watch the candidate, while repository \
         contents stay read-only because RELEASE_TOKEN alone moves refs"
    );
    assert!(
        !code.contains("contents: write"),
        "do not give GITHUB_TOKEN contents write: RELEASE_TOKEN is the narrowly configured, \
         trigger-capable credential for main and tag pushes"
    );
}
