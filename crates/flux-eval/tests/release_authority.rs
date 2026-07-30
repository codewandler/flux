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
