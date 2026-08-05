//! The authority envelope around the release programs (C-251).
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
//! 4. **The automatic cut has no model at all.** `examples/release-cut.flux` is the workflow entry
//!    point and contains no `task`, provider, network or changelog-writing op. The older optional
//!    release-note drafting example remains tool-less and is not reachable from release CI.
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

/// Every operation one checked-in release program calls. Collected from the serialized AST rather
/// than a hand-written match, so a new node kind cannot hide a call from this check.
fn ops_called_by_release_program(name: &str) -> BTreeSet<String> {
    let path = repo_root().join("examples").join(name);
    let src =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let ast = match flux_flow::program::Module::parse_str(&src)
        .unwrap_or_else(|e| panic!("{name} parses: {e}"))
    {
        flux_flow::program::Module::Flow(ast) => ast,
        flux_flow::program::Module::Program(p) => p
            .flows
            .first()
            .cloned()
            .expect("release program has a flow"),
    };
    let mut found = BTreeSet::new();
    collect_ops(
        &serde_json::to_value(&ast).expect("AST serializes"),
        &mut found,
    );
    found
}

fn ops_called_by_release_flux() -> BTreeSet<String> {
    ops_called_by_release_program("release.flux")
}

fn ops_called_by_automatic_release_flux() -> BTreeSet<String> {
    ops_called_by_release_program("release-cut.flux")
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
fn the_automatic_release_program_is_exactly_host_only() {
    let called = ops_called_by_automatic_release_flux();
    let expected = ["release_cut", "release_plan", "release_verify_versions"]
        .into_iter()
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        called, expected,
        "examples/release-cut.flux is the unattended entry point: it may derive, validate and cut, but never call a model, network op or general write/process tool"
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
        "the release-branch path must run the deterministic release cut with apply=true; a push-triggered \
         preview creates no commit or tag and therefore releases nothing"
    );
}

/// The cut reaches canonical main only after an exact-SHA dispatch of the full CI workflow. The
/// controller constructs one two-parent merge whose first parent is live main and pushes that
/// fast-forward with the PAT; the resulting SHA is then the candidate and one-time PAT tag target.
#[test]
fn the_release_workflow_prepares_an_exact_sha_candidate_before_pushing_the_tag() {
    let code = release_flow_workflow_code();
    let ci = workflow_code("ci.yml");
    assert!(
        ci.contains("workflow_dispatch:"),
        "the controller cannot gate the exact staged cut unless ci.yml accepts a no-input dispatch"
    );
    let stages = [
        "CI_BASELINE=$(latest_run_id ci.yml)",
        "actions_gh workflow run ci.yml",
        "CI_RUN=$(wait_for_exact_dispatch_run ci.yml",
        "git commit-tree \"$EXPECTED_TREE\"",
        "git_with_release_token push \"$PUSH_URL\" \"$MERGED_SHA:refs/heads/main\"",
        "merged main does not contain the exact cut diff",
        "\"$MERGED_SHA:$CANDIDATE_REF\"",
        "scripts/release-candidate.sh verify",
        "git mktag",
        "git_with_release_token push \"$PUSH_URL\" \"$tag_object:$TAG_REF\"",
        "wait_for_exact_run release.yml",
        "wait_for_exact_run crates-io.yml",
        "scripts/verify-github-release.sh --repo \"$GITHUB_REPOSITORY\" \"$TAG\"",
        "scripts/check-release-tags.sh --repo \"$GITHUB_REPOSITORY\"",
        "\":$CANDIDATE_REF\"",
    ];
    let indexes = stages
        .iter()
        .map(|stage| {
            code.find(stage)
                .unwrap_or_else(|| panic!("promotion is missing stage `{stage}`"))
        })
        .collect::<Vec<_>>();
    assert!(
        indexes.windows(2).all(|pair| pair[0] < pair[1]),
        "exact cut CI, merged-main candidate, PAT tag, exact runs, public/latest audit and cleanup must remain ordered: {indexes:?}"
    );
    assert!(!code.contains("HEAD:main") && !code.contains("--admin"));
    assert!(code.contains("[ -n \"${RELEASE_TOKEN:-}\" ]"));
    assert!(code.contains("[ -z \"${PROMOTION_TOKEN:-}\" ]"));
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

/// The unattended cut receives one complete exact-SHA CI run. The candidate workflow verifies that
/// immutable run instead of rebuilding and retesting the workspace, and may write a receipt only
/// after that verification; promotion verifies the receipt before either public ref moves.
#[test]
fn automated_release_gates_the_exact_candidate_once_before_promotion() {
    let flow = workflow_code("release-flow.yml");
    let release = release_workflow_code();
    let cut = non_comment_source(repo_root().join("scripts/cut-release.sh"));
    // Since C-355 the receipt format lives in one place, so that the writer, the verifier and the
    // promotion consumer cannot drift apart.
    let receipt_helper = non_comment_source(repo_root().join("scripts/candidate_artifacts.py"));

    assert!(
        flow.contains("FLUX_RELEASE_CANDIDATE_OWNS_GATE"),
        "the automatic release must explicitly delegate release-gate verification"
    );
    assert!(
        cut.contains("--no-gate")
            && cut.contains("GITHUB_ACTIONS")
            && cut.contains("GITHUB_EVENT_NAME")
            && cut.contains("refs/heads/release"),
        "cut-release must reject --no-gate outside the automated release-branch push"
    );

    let validate = code_line_index(&release, |line| {
        line.contains("Validate release-candidate request")
    });
    let gate = code_line_index(&release, |line| {
        line.contains("Verify the successful exact cut CI")
    });
    let receipt = code_line_index(&release, |line| {
        line.contains("scripts/release-candidate.sh write release-candidate.txt")
    });
    assert!(
        matches!((validate, gate, receipt), (Some(v), Some(g), Some(r)) if v < g && g < r),
        "release.yml must validate the candidate ref, verify exact cut CI, and only then write its receipt; indexes: validate={validate:?}, gate={gate:?}, receipt={receipt:?}"
    );
    assert!(
        receipt_helper.contains(r#"GATE = "mandatory-full-v1""#)
            && receipt_helper.contains(r#"f"gate={GATE}""#)
            && receipt_helper.contains(r#"f"gate_commit={commit}""#),
        "the immutable candidate receipt must bind the candidate admitted by the exact cut-CI gate"
    );
    assert!(
        receipt_helper.contains(r#"SCHEMA = "flux-release-candidate-v3""#),
        "the candidate receipt must be v3: v2 binds no artifact identities or digests (C-355)"
    );
}

/// C-355. The receipt authenticates the bytes, not just the run: the tag run must consume the
/// promotion source by immutable artifact ID through the verifying consumer, never by re-globbing
/// `artifacts-*` from the candidate run and trusting the download action's merge.
#[test]
fn the_tag_run_consumes_the_candidate_bytes_through_the_receipt() {
    let release = release_workflow_code();
    let verify = code_line_index(&release, |line| {
        line.contains("Verify and safely assemble the receipt-bound candidate bytes")
    });
    let fetch = code_line_index(&release, |line| {
        line.contains("scripts/release-candidate.sh fetch")
    });
    let dist_host = code_line_index(&release, |line| line.contains("dist host "));
    let staged = code_line_index(&release, |line| {
        line.contains("verify-github-release.sh --staged")
    });
    assert!(
        matches!((verify, fetch, dist_host, staged),
            (Some(verify), Some(fetch), Some(host), Some(staged))
                if verify < fetch && fetch < host && host < staged),
        "release.yml must verify and safely extract the receipt-bound bytes before `dist host` and \
         the staged asset check; found verify={verify:?}, fetch={fetch:?}, host={dist_host:?}, \
         staged={staged:?}"
    );

    let consumer = non_comment_source(repo_root().join("scripts/candidate_artifacts.py"));
    for stage in [
        "_check_metadata(record, metadata, run_id)",
        "_check_raw_bytes(record, raw)",
        "_check_zip_structure(record, raw_path)",
        "safe_extract(raw_path, namespaces / record.name, taken)",
    ] {
        assert!(
            consumer.contains(stage),
            "the promotion consumer must keep its `{stage}` stage"
        );
    }
    let order = [
        "_check_metadata(record, metadata, run_id)",
        "raw = downloader.download(record.identifier)",
        "_check_raw_bytes(record, raw)",
        "raw_path.write_bytes(raw)",
        "_check_zip_structure(record, raw_path)",
        "safe_extract(raw_path, namespaces / record.name, taken)",
    ]
    .map(|stage| consumer.find(stage).expect("consumer stage exists"));
    assert!(
        order.windows(2).all(|pair| pair[0] < pair[1]),
        "identity, then raw-byte digest, then ZIP structure, then namespaced extraction — hashing \
         after opening the archive authenticates a parse of the bytes, not the bytes: {order:?}"
    );
}

/// Release availability is a repository property, not a model-account property. The automatic cut
/// must call only the host-only program and must not accept or interpolate any provider credential.
#[test]
fn release_flow_is_credential_free_and_calls_only_the_host_cut() {
    let code = workflow_code("release-flow.yml");
    assert!(
        code.contains("flux flow run examples/release-cut.flux"),
        "release-flow.yml must run the deterministic host-only release program"
    );
    for forbidden in [
        "ANTHROPIC_API_KEY",
        "OPENROUTER_API_KEY",
        "OPENAI_API_KEY",
        "FLUX_SMOKE_MODEL",
        "scripts/smoke-live.sh",
        "inputs.model",
        "-m $RELEASE_MODEL",
        "examples/release.flux",
    ] {
        assert!(
            !code.contains(forbidden),
            "release-flow.yml must not depend on model/provider surface `{forbidden}`"
        );
    }
}

/// The deterministic Flux-Lang cut still runs fixed process operations through guarded System.
/// Hosted Ubuntu lacks bubblewrap by default, so prove the backend before entering the host flow.
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
    let flow = code_line_index(&code, |line| {
        line.contains("flux flow run examples/release-cut.flux")
    });
    assert!(
        matches!((backend, user_namespace, backend_probe, flow),
            (Some(backend), Some(userns), Some(probe), Some(flow))
                if backend < userns && userns < probe && probe < flow),
        "release-flow.yml must install bubblewrap, enable the hosted Ubuntu user-namespace \
         primitive, and self-test the backend before the deterministic Flux flow; found \
         install={backend:?}, userns={user_namespace:?}, probe={backend_probe:?}, flow={flow:?}"
    );
}

/// `cut-release.sh` regenerates the embedded documentation snapshot, which invokes the website's
/// local Docusaurus binary. Hosted runners start without `website/node_modules`, so the automatic
/// release must install the locked website toolchain before it enters the deterministic cut.
#[test]
fn release_flow_installs_the_locked_docs_toolchain_before_running_flux() {
    let code = workflow_code("release-flow.yml");
    let node = code_line_index(&code, |line| line.contains("actions/setup-node@"));
    let lockfile = code_line_index(&code, |line| {
        line.contains("cache-dependency-path: website/package-lock.json")
    });
    let install = code_line_index(&code, |line| line.trim() == "- run: npm ci");
    let install_directory =
        code_line_index(&code, |line| line.trim() == "working-directory: website");
    let flow = code_line_index(&code, |line| {
        line.contains("flux flow run examples/release-cut.flux")
    });
    assert!(
        matches!((node, lockfile, install, install_directory, flow),
            (Some(node), Some(lockfile), Some(install), Some(directory), Some(flow))
                if node < lockfile && lockfile < install && install < directory && directory < flow),
        "release-flow.yml must provision pinned Node and install website/package-lock.json before \
         the deterministic cut can regenerate embedded docs; found node={node:?}, lockfile={lockfile:?}, \
         install={install:?}, directory={install_directory:?}, flow={flow:?}"
    );
}

/// C-559 deliberately removes the unconfigured App and GitHub Environments from the release path.
/// Keep this as a whole-inventory assertion: a future release workflow must not quietly restore a
/// settings dependency that canonical `main` cannot satisfy.
#[test]
fn release_workflows_require_no_app_or_environment_settings() {
    for name in [
        "release.yml",
        "release-flow.yml",
        "release-plugins.yml",
        "crates-io.yml",
    ] {
        let workflow = workflow_code(name);
        for forbidden in [
            "PROMOTION_APP_ID",
            "PROMOTION_APP_PRIVATE_KEY",
            "scripts/mint-promotion-token.sh",
            "environment: release-control",
            "environment: release",
        ] {
            assert!(
                !workflow.contains(forbidden),
                "{name} still depends on removed release setting `{forbidden}`"
            );
        }
    }
    assert!(
        !repo_root().join("scripts/mint-promotion-token.sh").exists(),
        "App-token minting must be removed with the App settings"
    );
}

/// GitHub deliberately suppresses workflow runs caused by refs pushed with `GITHUB_TOKEN`, so the
/// promotion path needs a separately configured credential for git refs; otherwise a green auto-cut
/// silently publishes nothing. C-559 uses the existing repository `RELEASE_TOKEN` for ref movement
/// and the job-scoped Actions token only for exact workflow dispatch and observation.
#[test]
fn promotion_uses_the_step_scoped_release_token_outside_the_cut_job() {
    let workflow = workflow_code("release-flow.yml");
    assert_eq!(
        workflow.matches("secrets.RELEASE_TOKEN").count(),
        1,
        "release-flow.yml must pass RELEASE_TOKEN to exactly the host-owned promotion step"
    );
    assert!(
        workflow.contains("actions: write") && workflow.contains("contents: read"),
        "the controller needs Actions write for exact CI/candidate dispatch while repository \
         contents stay read-only because only the step-scoped PAT moves refs"
    );
    assert!(
        !workflow.contains("contents: write"),
        "do not give GITHUB_TOKEN contents write: RELEASE_TOKEN is the trigger-capable credential"
    );

    // The deterministic cut half must not be able to reach the promotion identity, which is a
    // statement about the job boundary rather than about the file.
    let cut_job = workflow
        .split("\n  release-control:")
        .next()
        .expect("release-flow.yml must contain a release-control job");
    for credential in ["PROMOTION_TOKEN", "RELEASE_TOKEN"] {
        assert!(
            !cut_job.contains(credential),
            "the deterministic plan/cut job must not reference {credential}"
        );
    }

    let promoter = non_comment_source(repo_root().join("scripts/promote-release-flow.sh"));
    for required in [
        "RELEASE_CAN_PUSH=$(release_gh api",
        "git_with_release_token push \"$PUSH_URL\" \"$CUT_SHA:$CUT_REF\"",
        "git_with_release_token push \"$PUSH_URL\" \"$MERGED_SHA:refs/heads/main\"",
        "git_with_release_token push \"$PUSH_URL\" \"$MERGED_SHA:$CANDIDATE_REF\"",
        "git_with_release_token push \"$PUSH_URL\" \"$tag_object:$TAG_REF\"",
    ] {
        assert!(
            promoter.contains(required),
            "promotion helper is missing RELEASE_TOKEN boundary `{required}`"
        );
    }
    assert!(
        promoter.contains("actions_gh workflow run ci.yml")
            && !promoter.contains("actions_gh pr create")
            && !promoter.contains("actions_gh pr merge")
            && !promoter.contains("actions_gh api -X"),
        "ambient GITHUB_TOKEN may dispatch/observe exact Actions runs but never mutates pull \
         requests or git refs"
    );
}
