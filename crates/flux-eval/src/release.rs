//! Release ops for `examples/release.flux` (C-251) — the **host half** of an automated release cut.
//!
//! The division of labour this module exists to enforce: **the model writes prose, the host decides
//! the version.** crates.io is yank-only, so a model that reads a diff and calls a breaking change
//! "patch" has published it permanently under a compatible version number, across 30 crate names.
//! The signal it would be guessing at is already mechanical — this repo marks breaking commits with a
//! conventional-commit `!` (`feat(capabilities,tools)!:`, `refactor(events,sdk,cli)!:`) and the rule
//! is *breaking → MINOR while `0.y`, additive and fixes → patch*. That is a regex, not a judgement,
//! so [`derive_bump`] is a regex and nothing reads a version back out of a model reply.
//!
//! ## Why these are ops and not `proc.run`
//!
//! `proc.run` grants **arbitrary** process authority: its permission subject is whatever program the
//! caller names. A release flow needs exactly two programs, so it gets two ops with **fixed argv**
//! ([`ReleaseVerifyVersionsTool`], [`ReleaseCutTool`]). The program therefore never holds general
//! process authority at all — "process authority scoped to the named scripts" is a property of the
//! op set, not of a policy rule that could be mis-typed. Same reasoning for
//! [`ChangelogInsertTool`] versus `write`: it can only address the three changelog files, so the
//! release flow never holds general write authority.
//!
//! ## What deliberately stays outside
//!
//! Pushing, the GitHub release, and crates.io. [`ReleaseCutTool`] stops at the **local annotated
//! tag** `scripts/cut-release.sh` creates; CI promotes it (BUILD-ONCE candidate → promote). Keeping
//! the irreversible half in CI means a bug in this module cannot publish. The version *math* and the
//! transactional roll also stay in `scripts/cut-release.sh`: it snapshots every file it touches and
//! restores them on any non-zero exit (C-147), so a red gate leaves no phantom version section. This
//! module calls it and checks its answer — it does not reimplement it, because a second
//! implementation of the roll is exactly how the 0.14.3 phantom-section class recurs.

use std::sync::LazyLock;
use std::time::Duration;

use async_trait::async_trait;
use regex::Regex;
use serde_json::{json, Value};

use flux_core::{Error, Result};
use flux_runtime::{Tool, ToolContext, ToolResult};
use flux_spec::{
    tool_input_schema, tool_output_schema, AccessKind, Effect, Idempotency, Intent, IntentBehavior,
    IntentCertainty, IntentRole, IntentSet, IntentTarget, Risk, ToolSpec,
};
use flux_system::PathAccess;

use crate::util::{json_result, parse_params};

/// The only files the release flow may write prose into. [`ChangelogInsertTool`] refuses anything
/// else **structurally**: the target is normalized through `flux-system`'s canonicalizing IO
/// boundary and then compared against this list, so `./CHANGELOG.md`, `docs/../CHANGELOG.md` and a
/// traversal all resolve before the comparison rather than after it.
///
/// `website/docs/whats-new.md` is listed because it is the tested mirror of `WHATS-NEW.md`
/// (`website_customer_changelog_is_in_sync`) — a bare `WHATS-NEW.md` edit reds the gate until it is
/// regenerated. In practice `scripts/cut-release.sh` regenerates it in the release commit; the entry
/// exists so the authority the flow *declares* matches the files a release touches.
pub const WRITABLE_CHANGELOGS: &[&str] =
    &["CHANGELOG.md", "WHATS-NEW.md", "website/docs/whats-new.md"];

/// The scripts the release flow may run, and the op that owns each. Both are fixed argv — nothing
/// here takes a program name from a caller.
pub const RELEASE_SCRIPTS: &[&str] = &["scripts/check-crate-versions.sh", "scripts/cut-release.sh"];

/// A conventional-commit breaking marker on a subject line: `type!:` or `type(scope)!:`. Anchored at
/// the start of a line because a `!` anywhere else in a subject is prose, not a signal.
static BREAKING_TITLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z][A-Za-z0-9_-]*(\([^()\r\n]+\))?!:")
        .expect("breaking-title pattern compiles")
});

/// The `BREAKING CHANGE:` / `BREAKING:` footer form, which some commits use instead of `!`.
static BREAKING_FOOTER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(BREAKING CHANGE|BREAKING-CHANGE|BREAKING):[^\r\n]*\S[^\r\n]*$")
        .expect("footer compiles")
});

static COMMIT_SHA: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[0-9a-f]{40}$").expect("commit SHA compiles"));

static TRAILER_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z0-9-]+(?: [A-Za-z0-9-]+)*:[^\r\n]*$").expect("trailer line compiles")
});

const COMMIT_RECORD_SEPARATOR: char = '\u{1e}';

#[derive(Debug, Clone, serde::Serialize)]
pub struct ReleaseCommitRecord {
    pub sha: String,
    pub message: String,
}

/// Decode `git log --format=%H%x00%B%x00%x1e` output. The NUL-delimited fields and explicit record
/// separator keep an unterminated body, a footer-looking next subject, and embedded newlines from
/// changing commit boundaries.
pub fn parse_commit_records(raw: &str) -> Result<Vec<ReleaseCommitRecord>> {
    let mut records = Vec::new();
    for raw_record in raw.split(COMMIT_RECORD_SEPARATOR) {
        let record = raw_record.trim_matches(['\r', '\n']);
        if record.is_empty() {
            continue;
        }
        let Some((sha, rest)) = record.split_once('\0') else {
            return Err(Error::Other(
                "release log record has no SHA delimiter".into(),
            ));
        };
        let Some(message) = rest.strip_suffix('\0') else {
            return Err(Error::Other(format!(
                "release log record {sha} has no message terminator"
            )));
        };
        if !COMMIT_SHA.is_match(sha) || message.contains('\0') {
            return Err(Error::Other(format!(
                "release log record has invalid framing or SHA `{sha}`"
            )));
        }
        records.push(ReleaseCommitRecord {
            sha: sha.to_string(),
            message: message.to_string(),
        });
    }
    Ok(records)
}

fn footer_block(message: &str) -> Vec<&str> {
    let lines: Vec<&str> = message.trim_end_matches(['\r', '\n']).lines().collect();
    if lines.len() == 1 && BREAKING_FOOTER.is_match(lines[0]) {
        return lines;
    }
    let Some(blank) = lines.iter().rposition(|line| line.trim().is_empty()) else {
        return Vec::new();
    };
    let candidate = &lines[blank + 1..];
    if candidate.is_empty()
        || candidate.iter().any(|line| {
            !line.starts_with(' ')
                && !line.starts_with('\t')
                && !TRAILER_LINE.is_match(line.trim_end())
        })
    {
        return Vec::new();
    }
    candidate.to_vec()
}

/// Breaking markers from one complete commit message. Subject and footer positions are evaluated
/// independently so incidental body prose and adjacent records cannot become a signal.
pub fn commit_breaking_markers(message: &str) -> Vec<String> {
    let normalized = message.replace("\r\n", "\n");
    let mut markers = Vec::new();
    if let Some(subject) = normalized.lines().next().map(str::trim_end) {
        if BREAKING_TITLE.is_match(subject) {
            markers.push(subject.to_string());
        }
    }
    markers.extend(
        footer_block(&normalized)
            .into_iter()
            .map(str::trim_end)
            .filter(|line| BREAKING_FOOTER.is_match(line))
            .map(str::to_string),
    );
    markers
}

/// Whether the current customer-facing release notes contain a non-empty migration section.
pub fn unreleased_action_needed(whats_new: &str) -> bool {
    let mut in_unreleased = false;
    let mut in_action = false;
    for line in whats_new.lines() {
        let heading = line.trim();
        if heading == "## [Unreleased]" {
            in_unreleased = true;
            in_action = false;
            continue;
        }
        if in_unreleased && heading.starts_with("## [") {
            break;
        }
        if !in_unreleased {
            continue;
        }
        if heading.starts_with("### ") {
            in_action = heading == "### Action needed";
            continue;
        }
        if in_action && !heading.is_empty() && !heading.starts_with("<!--") {
            return true;
        }
    }
    false
}

pub fn derive_release_bump(
    commits: &[ReleaseCommitRecord],
    action_needed: bool,
    current_version: &str,
) -> &'static str {
    let breaking = action_needed
        || commits
            .iter()
            .any(|record| !commit_breaking_markers(&record.message).is_empty());
    if !breaking {
        "patch"
    } else if current_version.starts_with("0.") {
        "minor"
    } else {
        "major"
    }
}

/// The first `version = "X.Y.Z"` line of a manifest — the same `grep -m1 '^version = '` that
/// `scripts/cut-release.sh` reads, so the host's prediction and the script's answer come from one
/// definition of "current version".
static MANIFEST_VERSION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?m)^version = "([0-9]+\.[0-9]+\.[0-9]+)""#).expect("version pattern compiles")
});

/// The line `scripts/cut-release.sh` prints once the cut is in history.
static CUT_ANNOUNCEMENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"== cut v([0-9]+\.[0-9]+\.[0-9]+)").expect("cut line compiles"));

/// Every subject line in `log` that carries a breaking signal. Returned rather than counted so a
/// disagreement or a review can see *which* commit forced the bump.
pub fn breaking_titles(log: &str) -> Vec<String> {
    log.lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .filter(|line| BREAKING_TITLE.is_match(line) || BREAKING_FOOTER.is_match(line))
        .map(str::to_string)
        .collect()
}

/// The bump `log` implies, under flux's rule: **breaking → minor while `0.y`**, additive and fixes →
/// patch. Past 1.0 the same signal means `major`; the branch is written out rather than assumed
/// because the repo will cross that line eventually and a silent mis-read there is unrecoverable.
pub fn derive_bump(log: &str, current_version: &str) -> &'static str {
    if breaking_titles(log).is_empty() {
        return "patch";
    }
    if current_version.starts_with("0.") {
        "minor"
    } else {
        "major"
    }
}

/// Apply `bump` to `current`, exactly as `scripts/cut-release.sh` does.
pub fn next_version(current: &str, bump: &str) -> Result<String> {
    let parts: Vec<u64> = current
        .split('.')
        .map(|p| p.parse::<u64>().map_err(|_| ()))
        .collect::<std::result::Result<Vec<_>, ()>>()
        .map_err(|()| Error::Other(format!("unparseable current version `{current}`")))?;
    let [major, minor, patch] = parts.as_slice() else {
        return Err(Error::Other(format!(
            "current version `{current}` is not X.Y.Z"
        )));
    };
    Ok(match bump {
        "patch" => format!("{major}.{minor}.{}", patch + 1),
        "minor" => format!("{major}.{}.0", minor + 1),
        "major" => format!("{}.0.0", major + 1),
        other => {
            return Err(Error::Other(format!(
                "unknown bump `{other}` (expected patch|minor|major)"
            )))
        }
    })
}

/// The workspace version from a root `Cargo.toml`.
pub fn manifest_version(manifest: &str) -> Option<String> {
    MANIFEST_VERSION
        .captures(manifest)
        .map(|c| c[1].to_string())
}

/// Read the root manifest's version through guarded IO.
async fn current_version(ctx: &ToolContext) -> Result<String> {
    let manifest = ctx.system().read_file("Cargo.toml").await?;
    manifest_version(&manifest).ok_or_else(|| {
        Error::Other("Cargo.toml has no `version = \"X.Y.Z\"` line to bump from".into())
    })
}

/// Run `git` with `args` in the workspace, returning trimmed stdout. A non-zero exit is an error
/// carrying stderr — ground truth that cannot be read is a halt, never an empty string that would
/// silently derive `patch`.
async fn git(ctx: &ToolContext, args: &[&str]) -> Result<String> {
    let mut argv = vec!["git".to_string()];
    argv.extend(args.iter().map(|a| a.to_string()));
    let out = ctx.system().run(&argv, Duration::from_secs(60)).await?;
    if out.exit_code != 0 {
        return Err(Error::Other(format!(
            "git {} failed [exit {}]: {}",
            args.join(" "),
            out.exit_code,
            out.stderr.trim()
        )));
    }
    Ok(out.stdout.trim().to_string())
}

async fn git_framed_log(ctx: &ToolContext, range: &str) -> Result<String> {
    let argv = vec![
        "git".to_string(),
        "log".to_string(),
        range.to_string(),
        "--format=%H%x00%B%x00%x1e".to_string(),
    ];
    let out = ctx.system().run(&argv, Duration::from_secs(60)).await?;
    if out.exit_code != 0 {
        return Err(Error::Other(format!(
            "git log {range} failed [exit {}]: {}",
            out.exit_code,
            out.stderr.trim()
        )));
    }
    Ok(out.stdout)
}

// ---------------------------------------------------------------------------
// release_plan
// ---------------------------------------------------------------------------

/// `release_plan()` — the single place a release version can come from.
pub struct ReleasePlanTool;

/// Arguments for the `release_plan` op (none).
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReleasePlanInput {}

#[async_trait]
impl Tool for ReleasePlanTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "release_plan".into(),
            description: "Derive the release plan from git and Cargo.toml: the last `v*` tag, the \
                          commit subjects and diffstat since it, and the HOST's bump decision \
                          (conventional-commit `!` ⇒ minor while 0.y, else patch) with the next \
                          version. Returns {last_tag, range, commit_count, log, diffstat, breaking, \
                          bump, current_version, next_version}. Never reads a version from a model."
                .into(),
            input_schema: tool_input_schema::<ReleasePlanInput>(),
            output_schema: None,
            effects: vec![Effect::Read, Effect::Process],
            risk: Risk::Medium,
            // Repeatable, never replayable: the answer tracks the repo, not the (empty) input, so a
            // cached plan for a tree that has since moved is the worst possible answer.
            idempotency: Idempotency::Conditional,
            access: vec![AccessKind::Process],
            group: None,
        }
    }

    fn permission_subjects(&self, _params: &Value) -> Vec<String> {
        vec!["git".to_string()]
    }

    fn intents(&self, _params: &Value) -> IntentSet {
        let mut set = IntentSet::new();
        set.push(Intent {
            behavior: IntentBehavior::CommandExecution,
            target: IntentTarget::Process {
                command: "git log".to_string(),
            },
            role: IntentRole::ProcessCommand,
            certainty: IntentCertainty::Certain,
        });
        set
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let _: ReleasePlanInput = parse_params(&params, "release_plan")?;

        let last_tag = git(ctx, &["describe", "--tags", "--abbrev=0", "--match", "v*"]).await?;
        let range = format!("{last_tag}..HEAD");
        let commit_count: u64 = git(ctx, &["rev-list", "--count", &range])
            .await?
            .parse()
            .map_err(|_| Error::Other(format!("could not count commits in {range}")))?;
        let commits = parse_commit_records(&git_framed_log(ctx, &range).await?)?;
        if commits.len() as u64 != commit_count {
            return Err(Error::Other(format!(
                "release log framing produced {} records for {commit_count} commits in {range}",
                commits.len()
            )));
        }
        let log = commits
            .iter()
            .map(|record| record.message.trim_end())
            .collect::<Vec<_>>()
            .join("\n\n");
        let diffstat = git(ctx, &["diff", "--stat", &range]).await?;

        let current = current_version(ctx).await?;
        let whats_new = ctx.system().read_file("WHATS-NEW.md").await?;
        let action_needed = unreleased_action_needed(&whats_new);
        let bump = derive_release_bump(&commits, action_needed, &current);
        let next = next_version(&current, bump)?;
        let breaking = commits
            .iter()
            .flat_map(|record| {
                commit_breaking_markers(&record.message)
                    .into_iter()
                    .map(|marker| format!("{} {marker}", record.sha))
            })
            .collect::<Vec<_>>();

        let view = format!(
            "{last_tag} → {next} ({bump}); {commit_count} commit(s), {} breaking",
            breaking.len()
        );
        json_result(
            &json!({
                "last_tag": last_tag,
                "range": range,
                "commit_count": commit_count,
                "commits": commits,
                "log": log,
                "diffstat": diffstat,
                "breaking": breaking,
                "action_needed": action_needed,
                "bump": bump,
                "current_version": current,
                "next_version": next,
            }),
            view,
        )
    }
}

// ---------------------------------------------------------------------------
// release_verify_versions
// ---------------------------------------------------------------------------

/// `release_verify_versions()` — fixed-argv `scripts/check-crate-versions.sh`.
///
/// The protocol-line crates (`codewandler-flux-{spec,secret,policy,evidence,datasource,
/// plugin-protocol,host-kit}`) version **the wire** on their own `1.x` line (C-143). A model must
/// never reason about wire compatibility, so when this fails the flow's job is to stop with the crate
/// named — not to guess, and not to continue to a cut.
pub struct ReleaseVerifyVersionsTool;

/// Arguments for the `release_verify_versions` op (none).
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReleaseVerifyVersionsInput {}

/// Crate names from the script's `FAIL <name> changed since <base>` lines.
fn offending_crates(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| line.trim().strip_prefix("FAIL "))
        .filter_map(|rest| rest.split_whitespace().next())
        .map(str::to_string)
        .collect()
}

#[async_trait]
impl Tool for ReleaseVerifyVersionsTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "release_verify_versions".into(),
            description: "Run `scripts/check-crate-versions.sh` (fixed argv) and report whether the \
                          independently-versioned protocol-line crates are correctly bumped. \
                          Returns {ok, output} when clean and ERRORS with the offending crate named \
                          when not — a wire version is a human decision, so a failure halts the cut."
                .into(),
            input_schema: tool_input_schema::<ReleaseVerifyVersionsInput>(),
            output_schema: None,
            effects: vec![Effect::Process, Effect::LocalSystem],
            risk: Risk::Medium,
            idempotency: Idempotency::Conditional,
            access: vec![AccessKind::Process, AccessKind::LocalSystem],
            group: None,
        }
    }

    fn permission_subjects(&self, _params: &Value) -> Vec<String> {
        vec![RELEASE_SCRIPTS[0].to_string()]
    }

    fn intents(&self, _params: &Value) -> IntentSet {
        let mut set = IntentSet::new();
        set.push(Intent {
            behavior: IntentBehavior::CommandExecution,
            target: IntentTarget::Process {
                command: RELEASE_SCRIPTS[0].to_string(),
            },
            role: IntentRole::ProcessCommand,
            certainty: IntentCertainty::Certain,
        });
        set
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let _: ReleaseVerifyVersionsInput = parse_params(&params, "release_verify_versions")?;
        let argv = vec![RELEASE_SCRIPTS[0].to_string()];
        let out = ctx.system().run(&argv, Duration::from_secs(600)).await?;
        let output = format!("{}{}", out.stdout, out.stderr).trim().to_string();
        if out.exit_code != 0 {
            // An error, not a `{ok: false}` datum. This must abort the flow *before* any changelog
            // is touched, and the crate name has to travel with the halt — a caller that has to
            // remember to check a boolean is a caller that will one day forget.
            let offending = offending_crates(&output);
            let named = if offending.is_empty() {
                "no crate named in the output".to_string()
            } else {
                offending.join(", ")
            };
            return Ok(ToolResult::error(format!(
                "protocol-line version check FAILED [exit {}] — {named} changed without a version \
                 bump. The plugin protocol line is SemVer over the WIRE and a human must decide it; \
                 refusing to cut.\n{output}",
                out.exit_code
            )));
        }
        json_result(
            &json!({ "ok": true, "output": output }),
            "protocol-line crate versions are clean",
        )
    }
}

// ---------------------------------------------------------------------------
// release_parse_notes
// ---------------------------------------------------------------------------

/// The exact model-to-host contract for release prose.
///
/// `task()` intentionally returns text, even when its prompt requests JSON. The host parses that
/// text at one explicit boundary before Flux-Lang reads fields from it. Unknown and missing fields
/// fail closed, as does explanatory prose around the JSON object. One canonical `json` Markdown
/// fence is normalized because the hosted scribe produced that exact transport wrapper despite the
/// no-fence instruction; the object inside still crosses the same strict schema.
#[derive(Debug, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReleaseNotes {
    changelog: String,
    whats_new: String,
    bump_opinion: BumpOpinion,
    bump_reason: String,
}

#[derive(Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum BumpOpinion {
    Patch,
    Minor,
}

/// Parse and validate the release scribe's textual result.
fn parse_release_notes(text: &str) -> Result<ReleaseNotes> {
    let text = text.trim();
    let json = text
        .strip_prefix("```json\n")
        .and_then(|body| body.strip_suffix("\n```"))
        .or_else(|| {
            text.strip_prefix("```json\r\n")
                .and_then(|body| body.strip_suffix("\r\n```"))
        })
        .unwrap_or(text);
    let notes: ReleaseNotes = serde_json::from_str(json)
        .map_err(|e| Error::Other(format!("release_parse_notes: invalid scribe JSON: {e}")))?;
    validate_release_notes(notes)
}

fn validate_release_notes(notes: ReleaseNotes) -> Result<ReleaseNotes> {
    if notes.changelog.trim().is_empty() {
        return Err(Error::Other(
            "release_parse_notes: `changelog` must not be empty".into(),
        ));
    }
    Ok(notes)
}

/// `release_parse_notes(text)` — pure, strict adaptation of the scribe's text into typed fields.
pub struct ReleaseParseNotesTool;

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReleaseParseNotesInput {
    /// Exact JSON returned by the release-scribe task, either as raw text or runtime-decoded JSON.
    text: ReleaseNotesPayload,
}

/// Flux-Lang preserves ordinary task output as text, but decodes JSON-looking values while mapping
/// them into an operation argument. Both paths cross the same exact `ReleaseNotes` schema here.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
enum ReleaseNotesPayload {
    Text(String),
    Decoded(ReleaseNotes),
}

#[async_trait]
impl Tool for ReleaseParseNotesTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "release_parse_notes".into(),
            description: "Validate the release-scribe task's output as one exact JSON object. \
                          Accepts raw text, one canonical json Markdown fence, or Flux-Lang's \
                          decoded JSON value; rejects surrounding prose, missing/extra fields, \
                          empty engineering notes, and bump opinions outside patch|minor. Returns \
                          {changelog, whats_new, bump_opinion, bump_reason}. Pure: grants no \
                          filesystem, process, or network authority."
                .into(),
            input_schema: tool_input_schema::<ReleaseParseNotesInput>(),
            output_schema: Some(tool_output_schema::<ReleaseNotes>()),
            effects: vec![],
            risk: Risk::Low,
            idempotency: Idempotency::Idempotent,
            access: vec![],
            group: None,
        }
    }

    fn permission_subjects(&self, _params: &Value) -> Vec<String> {
        vec![]
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let args: ReleaseParseNotesInput = parse_params(&params, "release_parse_notes")?;
        let notes = match args.text {
            ReleaseNotesPayload::Text(text) => parse_release_notes(&text)?,
            ReleaseNotesPayload::Decoded(notes) => validate_release_notes(notes)?,
        };
        json_result(
            &serde_json::to_value(&notes).map_err(|e| Error::Other(e.to_string()))?,
            "release-scribe JSON validated",
        )
    }
}

// ---------------------------------------------------------------------------
// changelog_insert
// ---------------------------------------------------------------------------

/// `changelog_insert(file, body, section?, apply?)` — the resolved seam for getting model prose into
/// a changelog.
///
/// **The model must not write the file.** If it did, its output would become the file *content*
/// rather than the file's *input*, and one prompt injection or hallucinated section edits the release
/// notes. So the host owns the insertion: a deterministic anchor on `## [<section>]`, an
/// idempotence check, and a hard path allow-list ([`WRITABLE_CHANGELOGS`]).
pub struct ChangelogInsertTool;

/// Arguments for the `changelog_insert` op.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ChangelogInsertInput {
    /// Changelog to edit — must be one of the three release changelogs.
    file: String,
    /// Markdown to insert under the section heading.
    body: String,
    /// Section label to anchor on (default `Unreleased`).
    #[serde(default)]
    section: Option<String>,
    /// Write the file. Omitted or false **previews**: the diff is returned and nothing is written.
    #[serde(default)]
    apply: Option<bool>,
}

/// Insert `body` under the `## [<section>]` heading of `src`, returning the new document.
///
/// Deterministic and total: the anchor is a literal heading match, the body goes immediately after it
/// separated by one blank line, and everything else is byte-preserved. `None` means the anchor is
/// absent — a missing `[Unreleased]` heading is a halt, not an append, because appending would put
/// release notes in the wrong release.
pub fn insert_under_section(src: &str, section: &str, body: &str) -> Option<String> {
    let heading = format!("## [{section}]");
    let mut out = String::with_capacity(src.len() + body.len() + 2);
    let mut inserted = false;
    for line in src.split_inclusive('\n') {
        out.push_str(line);
        if !inserted && line.trim_end() == heading {
            if !line.ends_with('\n') {
                out.push('\n');
            }
            out.push('\n');
            out.push_str(body.trim_end());
            out.push('\n');
            inserted = true;
        }
    }
    inserted.then_some(out)
}

/// Whether `section` of `src` already carries `body` — the idempotence check that makes a re-run
/// after a failed cut safe instead of duplicating prose.
fn section_already_has(src: &str, section: &str, body: &str) -> bool {
    let heading = format!("## [{section}]");
    let Some(start) = src.find(&heading) else {
        return false;
    };
    let rest = &src[start + heading.len()..];
    let end = rest.find("\n## [").unwrap_or(rest.len());
    rest[..end].contains(body.trim())
}

/// A minimal unified-diff-shaped preview of an insertion: the anchor as context, the body as
/// additions. Enough for a human (or a workflow log) to see exactly what a dry run *would* apply.
fn insertion_preview(file: &str, section: &str, body: &str) -> String {
    let mut out = format!("--- a/{file}\n+++ b/{file}\n @@ ## [{section}] @@\n");
    out.push_str(&format!("  ## [{section}]\n+\n"));
    for line in body.trim_end().lines() {
        out.push('+');
        out.push_str(line);
        out.push('\n');
    }
    out
}

#[async_trait]
impl Tool for ChangelogInsertTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "changelog_insert".into(),
            description: "Insert markdown under a changelog's `## [<section>]` heading (default \
                          `Unreleased`), deterministically and idempotently. Only the three release \
                          changelogs are addressable. `apply` defaults to FALSE: the diff is \
                          returned and nothing is written. Returns {file, section, action, diff}."
                .into(),
            input_schema: tool_input_schema::<ChangelogInsertInput>(),
            output_schema: None,
            effects: vec![Effect::Write, Effect::Filesystem],
            risk: Risk::Medium,
            // Converges on repeat (the body is inserted once), but the outcome depends on the file
            // that is there — `Conditional`, never `Idempotent`, which would license a cache replay
            // in place of the write.
            idempotency: Idempotency::Conditional,
            access: vec![AccessKind::Filesystem],
            group: None,
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        serde_json::from_value::<ChangelogInsertInput>(crate::util::coerce_json(params))
            .map(|a| vec![a.file])
            .unwrap_or_default()
    }

    fn intents(&self, params: &Value) -> IntentSet {
        let mut set = IntentSet::new();
        if let Ok(a) =
            serde_json::from_value::<ChangelogInsertInput>(crate::util::coerce_json(params))
        {
            set.push(Intent {
                behavior: IntentBehavior::FilesystemWrite,
                target: IntentTarget::Path { path: a.file },
                role: IntentRole::WriteTarget,
                certainty: IntentCertainty::Certain,
            });
        }
        set
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let args: ChangelogInsertInput = parse_params(&params, "changelog_insert")?;
        let section = args.section.as_deref().unwrap_or("Unreleased");

        // Structural path scope. `path_identity` is flux-system's canonicalizing IO boundary: it
        // resolves `.`/`..` and symlinks and refuses anything outside the workspace, so the
        // comparison below happens on the physical target rather than on the caller's spelling.
        let identity = ctx
            .system()
            .path_identity(&args.file, PathAccess::Write)
            .map_err(|e| Error::Other(format!("changelog_insert: {e}")))?;
        if !WRITABLE_CHANGELOGS.contains(&identity.as_str()) {
            return Ok(ToolResult::error(format!(
                "changelog_insert refuses `{}` (resolved to `{identity}`): the release flow's write \
                 authority is exactly {} — prose never reaches source",
                args.file,
                WRITABLE_CHANGELOGS.join(", ")
            )));
        }

        let before = ctx.system().read_file(&args.file).await?;
        if section_already_has(&before, section, &args.body) {
            return json_result(
                &json!({
                    "file": args.file,
                    "section": section,
                    "action": "unchanged",
                    "diff": "",
                }),
                format!("{} already carries this [{section}] prose", args.file),
            );
        }

        let Some(after) = insert_under_section(&before, section, &args.body) else {
            return Ok(ToolResult::error(format!(
                "{} has no `## [{section}]` heading to anchor on — refusing to guess a location",
                args.file
            )));
        };
        let diff = insertion_preview(&args.file, section, &args.body);

        if args.apply.unwrap_or(false) {
            ctx.system().write_file(&args.file, &after).await?;
            json_result(
                &json!({
                    "file": args.file,
                    "section": section,
                    "action": "applied",
                    "diff": diff,
                }),
                format!("inserted under [{section}] in {}", args.file),
            )
        } else {
            json_result(
                &json!({
                    "file": args.file,
                    "section": section,
                    "action": "preview",
                    "diff": diff,
                }),
                format!("would insert under [{section}] in {}:\n{diff}", args.file),
            )
        }
    }
}

// ---------------------------------------------------------------------------
// release_cut
// ---------------------------------------------------------------------------

/// `release_cut(bump, apply?)` — fixed-argv `scripts/cut-release.sh`, and the host's cross-check on
/// its answer.
///
/// The script owns the mechanics (manifest sweep, re-lock, `[Unreleased]` roll, website mirror,
/// commit, annotated tag) and its own transactionality; this op adds one thing the script cannot do
/// for itself: it predicts the version from the manifest **before** running, and fails if the script
/// cut a different one. That closes the `sed`-collateral class (the global substitution that once
/// bumped an unrelated external crate sharing flux's version string).
pub struct ReleaseCutTool;

/// Arguments for the `release_cut` op.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReleaseCutInput {
    /// `patch`, `minor`, or `major` — the HOST's decision, from `release_plan`.
    bump: String,
    /// Actually cut. Omitted or false **previews**: the predicted version and argv are returned and
    /// no commit or tag is created.
    #[serde(default)]
    apply: Option<bool>,
}

#[async_trait]
impl Tool for ReleaseCutTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "release_cut".into(),
            description:
                "Cut a release with `scripts/cut-release.sh <bump>` (fixed argv): roll both \
                          changelogs, sweep versions, re-lock, commit, and create the LOCAL \
                          annotated tag. Never pushes and never publishes. `apply` defaults to \
                          FALSE: the predicted version is returned and nothing is cut. Returns \
                          {ok, action, version, tag, output}."
                    .into(),
            input_schema: tool_input_schema::<ReleaseCutInput>(),
            output_schema: None,
            effects: vec![Effect::Process, Effect::LocalSystem, Effect::Write],
            risk: Risk::High,
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::Process, AccessKind::LocalSystem],
            group: None,
        }
    }

    fn permission_subjects(&self, _params: &Value) -> Vec<String> {
        vec![RELEASE_SCRIPTS[1].to_string()]
    }

    fn intents(&self, _params: &Value) -> IntentSet {
        let mut set = IntentSet::new();
        set.push(Intent {
            behavior: IntentBehavior::CommandExecution,
            target: IntentTarget::Process {
                command: RELEASE_SCRIPTS[1].to_string(),
            },
            role: IntentRole::ProcessCommand,
            certainty: IntentCertainty::Certain,
        });
        set
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let args: ReleaseCutInput = parse_params(&params, "release_cut")?;
        let current = current_version(ctx).await?;
        // Predicted before the script runs, from the same rule the script applies — so a mismatch
        // afterwards is detectable rather than merely unlikely.
        let expected = next_version(&current, &args.bump)?;
        let mut argv = vec![RELEASE_SCRIPTS[1].to_string(), args.bump.clone()];

        if !args.apply.unwrap_or(false) {
            return json_result(
                &json!({
                    "ok": true,
                    "action": "preview",
                    "version": expected,
                    "tag": format!("v{expected}"),
                    "output": "",
                }),
                format!("would cut {current} → {expected} ({})", args.bump),
            );
        }

        // Humans and manual workflow rehearsals keep the transactional in-cut gate. Only the
        // host-owned automatic release-branch push delegates that work to release.yml, where the
        // exact cut SHA is gated once and earns an immutable receipt before promotion. The model
        // cannot select this branch: all four values come from the guarded host environment.
        let automated_candidate_gate = ctx
            .system()
            .env("FLUX_RELEASE_CANDIDATE_OWNS_GATE")
            .as_deref()
            == Some("true")
            && ctx.system().env("GITHUB_ACTIONS").as_deref() == Some("true")
            && ctx.system().env("GITHUB_EVENT_NAME").as_deref() == Some("push")
            && ctx.system().env("GITHUB_REF").as_deref() == Some("refs/heads/release");
        let out = if automated_candidate_gate {
            argv.push("--no-gate".to_string());
            let gate_owner_env = vec![
                (
                    "FLUX_RELEASE_CANDIDATE_OWNS_GATE".to_string(),
                    "true".to_string(),
                ),
                ("GITHUB_ACTIONS".to_string(), "true".to_string()),
                ("GITHUB_EVENT_NAME".to_string(), "push".to_string()),
                ("GITHUB_REF".to_string(), "refs/heads/release".to_string()),
            ];
            ctx.system()
                .run_with_env(&argv, &gate_owner_env, Duration::from_secs(5400))
                .await?
        } else {
            ctx.system().run(&argv, Duration::from_secs(5400)).await?
        };
        let output = format!("{}{}", out.stdout, out.stderr);
        if out.exit_code != 0 {
            let tail: String = output.chars().rev().take(1600).collect::<String>();
            let tail: String = tail.chars().rev().collect();
            return Ok(ToolResult::error(format!(
                "cut-release failed [exit {}] — the tree was restored (C-147), no phantom version \
                 section and no tag:\n…{}",
                out.exit_code,
                tail.trim()
            )));
        }
        let cut = CUT_ANNOUNCEMENT
            .captures(&output)
            .map(|c| c[1].to_string())
            .ok_or_else(|| {
                Error::Other(format!(
                    "cut-release exited 0 without announcing a version — refusing to claim a cut:\n{}",
                    output.trim()
                ))
            })?;
        if cut != expected {
            return Ok(ToolResult::error(format!(
                "cut-release cut {cut} but the host predicted {expected} from {current} + \
                 {} — refusing to report a version the host did not decide",
                args.bump
            )));
        }
        json_result(
            &json!({
                "ok": true,
                "action": "cut",
                "version": cut,
                "tag": format!("v{cut}"),
                "output": output.trim(),
            }),
            format!("cut {cut}; tag v{cut} is LOCAL — CI promotes it"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_conventional_commit_bang_is_the_breaking_signal() {
        // The exact shapes this repo's own history uses.
        let log = "feat(capabilities,tools)!: gate the capability surface\nfix(core): tidy";
        assert_eq!(breaking_titles(log).len(), 1);
        assert_eq!(derive_bump(log, "0.37.0"), "minor");

        let log = "refactor(events,sdk,cli)!: collapse the emitter seam";
        assert_eq!(derive_bump(log, "0.37.0"), "minor");
    }

    #[test]
    fn a_log_of_only_fixes_and_docs_is_a_patch() {
        let log = "fix(widget): restore cache invalidation\ndocs(readme): fix a link\nchore: tidy";
        assert!(breaking_titles(log).is_empty());
        assert_eq!(derive_bump(log, "0.37.0"), "patch");
    }

    #[test]
    fn a_breaking_footer_counts_too() {
        assert_eq!(
            derive_bump("BREAKING CHANGE: the wire moved", "0.37.0"),
            "minor"
        );
        assert_eq!(derive_bump("BREAKING: the wire moved", "0.37.0"), "minor");
    }

    #[test]
    fn complete_commit_records_keep_breaking_markers_in_their_valid_locations() {
        let framed = concat!(
            "1111111111111111111111111111111111111111\0fix(core): ordinary subject\n\n",
            "This prose says BREAKING CHANGE: incidentally.\0\x1e\n",
            "2222222222222222222222222222222222222222\0feat(api): revise wire\n\n",
            "Details.\n\nBREAKING-CHANGE: callers must migrate\0\x1e\n",
            "3333333333333333333333333333333333333333\0refactor(cli)!: remove legacy flag\0\x1e",
        );
        let records = parse_commit_records(framed).expect("valid framed git log");
        assert_eq!(records.len(), 3);
        assert!(records[0].message.contains("incidentally"));
        assert!(commit_breaking_markers(&records[0].message).is_empty());
        assert_eq!(
            commit_breaking_markers(&records[1].message),
            vec!["BREAKING-CHANGE: callers must migrate"]
        );
        assert_eq!(
            commit_breaking_markers(&records[2].message),
            vec!["refactor(cli)!: remove legacy flag"]
        );
        assert_eq!(derive_release_bump(&records, false, "0.55.0"), "minor");
    }

    #[test]
    fn record_boundaries_cannot_form_a_phantom_breaking_footer() {
        let framed = concat!(
            "1111111111111111111111111111111111111111\0fix: end with BREAKING\0\x1e",
            "2222222222222222222222222222222222222222\0CHANGE: ordinary next subject\0\x1e",
        );
        let records = parse_commit_records(framed).unwrap();
        assert_eq!(derive_release_bump(&records, false, "0.55.0"), "patch");
        assert!(parse_commit_records("badly-framed-record").is_err());
    }

    #[test]
    fn unreleased_action_needed_forces_the_pre_one_zero_minor() {
        let notice = "# What's new in flux\n\n## [Unreleased]\n\n### Fixed\n\n- fixed\n\n### Action needed\n\n- migrate now\n\n## [0.55.0]\n\n### Action needed\n\n- old migration\n";
        assert!(unreleased_action_needed(notice));
        let records =
            parse_commit_records("1111111111111111111111111111111111111111\0fix: safe patch\0\x1e")
                .unwrap();
        assert_eq!(derive_release_bump(&records, true, "0.55.0"), "minor");
        assert_eq!(
            next_version("0.55.0", derive_release_bump(&records, true, "0.55.0")).unwrap(),
            "0.56.0"
        );

        let empty = "## [Unreleased]\n\n### Action needed\n\n\n### Fixed\n- fixed\n\n## [0.55.0]\n### Action needed\n- historical only\n";
        assert!(!unreleased_action_needed(empty));
    }

    #[test]
    fn live_manifest_baseline_previews_the_next_release() {
        // Derive from the live manifest and customer changelog instead of pinning a version:
        // a hard pin cannot survive its own release cut, whose gate compiles this test after
        // the version bump and changelog roll.
        let manifest = include_str!("../../../Cargo.toml");
        let whats_new = include_str!("../../../WHATS-NEW.md");
        let current = manifest_version(manifest).expect("workspace version");
        let records = parse_commit_records(
            "1111111111111111111111111111111111111111\0fix: otherwise patch\0\x1e",
        )
        .unwrap();
        let action_needed = unreleased_action_needed(whats_new);
        let bump = derive_release_bump(&records, action_needed, &current);
        if current.starts_with("0.") {
            assert_eq!(bump, if action_needed { "minor" } else { "patch" });
        }
        let next = next_version(&current, bump).unwrap();
        assert_ne!(next, current);
    }

    #[test]
    fn all_supported_footer_tokens_are_exact_and_footer_only() {
        for token in ["BREAKING CHANGE", "BREAKING-CHANGE", "BREAKING"] {
            let message = format!("feat: change\n\nbody\n\n{token}: migration");
            assert_eq!(commit_breaking_markers(&message).len(), 1, "{token}");
        }
        for prose in [
            "fix: mention BREAKING CHANGE: here",
            "fix: ordinary\n\nA paragraph with BREAKING: incidental prose.",
            "fix: ordinary\n\nNOT BREAKING: migration",
            "fix: ordinary\n\nBREAKING CHANGE : malformed",
        ] {
            assert!(commit_breaking_markers(prose).is_empty(), "{prose}");
        }
    }

    #[test]
    fn a_bang_that_is_not_a_conventional_commit_marker_is_prose() {
        // The signal is anchored: a `!` in the middle of a subject is emphasis, not a contract
        // change. Getting this wrong publishes a phantom minor on every excited commit message.
        let log = "fix(core): finally! the cache is correct\ndocs: read this!";
        assert!(
            breaking_titles(log).is_empty(),
            "mid-subject `!` must not read as breaking"
        );
        assert_eq!(derive_bump(log, "0.37.0"), "patch");
    }

    #[test]
    fn breaking_past_one_zero_is_major_not_minor() {
        assert_eq!(derive_bump("feat(x)!: change", "1.4.0"), "major");
    }

    #[test]
    fn next_version_matches_the_cut_script() {
        assert_eq!(next_version("0.37.0", "patch").unwrap(), "0.37.1");
        assert_eq!(next_version("0.37.9", "minor").unwrap(), "0.38.0");
        assert_eq!(next_version("0.37.9", "major").unwrap(), "1.0.0");
        assert!(next_version("0.37.0", "sideways").is_err());
        assert!(next_version("nope", "patch").is_err());
    }

    #[test]
    fn manifest_version_reads_the_first_version_line() {
        let manifest = "[workspace.package]\nversion = \"0.37.0\"\nedition = \"2021\"\n\n\
                        [workspace.dependencies]\nserde = \"1.0.0\"\n";
        assert_eq!(manifest_version(manifest).as_deref(), Some("0.37.0"));
        assert_eq!(manifest_version("name = \"x\"\n"), None);
    }

    #[test]
    fn release_notes_parse_from_one_exact_json_object() {
        let notes = parse_release_notes(
            r####"{"changelog":"### Fixed\n- safe","whats_new":"### Fixed\n- safer","bump_opinion":"patch","bump_reason":"fix-only release"}"####,
        )
        .unwrap();
        assert_eq!(notes.changelog, "### Fixed\n- safe");
        assert_eq!(notes.whats_new, "### Fixed\n- safer");
        assert_eq!(notes.bump_opinion, BumpOpinion::Patch);
        assert_eq!(notes.bump_reason, "fix-only release");

        let internal_only = parse_release_notes(
            r####"{"changelog":"### Changed\n- internal","whats_new":"","bump_opinion":"patch","bump_reason":""}"####,
        )
        .unwrap();
        assert!(internal_only.whats_new.is_empty());
        assert!(internal_only.bump_reason.is_empty());

        let fenced = parse_release_notes(&format!(
            "```json\n{}\n```",
            r#"{"changelog":"c","whats_new":"w","bump_opinion":"minor","bump_reason":"breaking"}"#
        ))
        .unwrap();
        assert_eq!(fenced.bump_opinion, BumpOpinion::Minor);
    }

    #[test]
    fn release_notes_reject_prose_wrappers_or_schema_drift() {
        let valid =
            r#"{"changelog":"c","whats_new":"w","bump_opinion":"minor","bump_reason":"breaking"}"#;
        for invalid in [
            format!("Here are the notes: {valid}"),
            format!("{valid}\nHope this helps."),
            r#"{"changelog":"c","whats_new":"w","bump_opinion":"major","bump_reason":"breaking"}"#.into(),
            r#"{"changelog":"c","whats_new":"w","bump_opinion":"patch"}"#.into(),
            r#"{"changelog":"c","whats_new":"w","bump_opinion":"patch","bump_reason":"fix","extra":true}"#.into(),
            r#"{"changelog":"","whats_new":"w","bump_opinion":"patch","bump_reason":"fix"}"#.into(),
            "not json".into(),
        ] {
            assert!(
                parse_release_notes(&invalid).is_err(),
                "accepted malformed release notes: {invalid}"
            );
        }
    }

    #[test]
    fn insertion_anchors_on_the_section_and_preserves_everything_else() {
        let src = "# Changelog\n\n## [Unreleased]\n\n## [0.37.0] - 2026-07-30\n\n- old\n";
        let out = insert_under_section(src, "Unreleased", "### Fixed\n- a thing").unwrap();
        assert_eq!(
            out,
            "# Changelog\n\n## [Unreleased]\n\n### Fixed\n- a thing\n\n## [0.37.0] - 2026-07-30\n\n- old\n"
        );
        // The already-released section is untouched.
        assert!(out.contains("## [0.37.0] - 2026-07-30\n\n- old\n"));
    }

    #[test]
    fn a_missing_anchor_is_a_halt_not_an_append() {
        // Appending would file release notes under the wrong release. There is no safe fallback.
        assert!(insert_under_section("# Changelog\n\n## [0.37.0]\n", "Unreleased", "x").is_none());
    }

    #[test]
    fn insertion_is_idempotent_so_a_rerun_after_a_failed_cut_does_not_duplicate() {
        let src = "## [Unreleased]\n\n### Fixed\n- a thing\n\n## [0.37.0]\n";
        assert!(section_already_has(
            src,
            "Unreleased",
            "### Fixed\n- a thing"
        ));
        // …and the same body under a DIFFERENT section does not count as present.
        let other = "## [Unreleased]\n\n## [0.37.0]\n\n### Fixed\n- a thing\n";
        assert!(!section_already_has(
            other,
            "Unreleased",
            "### Fixed\n- a thing"
        ));
    }

    #[test]
    fn the_offending_protocol_crate_is_named_from_the_script_output() {
        let out = "== crate versions vs v0.37.0 ==\n\
                   FAIL codewandler-flux-spec changed since v0.37.0 but is still 1.4.0\n\
                   FAIL codewandler-flux-policy changed since v0.37.0 but is still 1.2.0\n";
        assert_eq!(
            offending_crates(out),
            vec!["codewandler-flux-spec", "codewandler-flux-policy"]
        );
        assert!(offending_crates("PASS 2 changed crate(s)").is_empty());
    }

    #[test]
    fn the_writable_set_is_exactly_the_three_release_changelogs() {
        // Pinned deliberately: widening this list widens the release flow's entire write authority,
        // and it should never happen without someone reading this test's name.
        assert_eq!(
            WRITABLE_CHANGELOGS,
            &["CHANGELOG.md", "WHATS-NEW.md", "website/docs/whats-new.md"]
        );
    }
}
