//! `flux-skill` — markdown knowledge packs discovered from skill directories.
//!
//! A skill is a `.md` file (or a directory containing `SKILL.md`) with YAML frontmatter. flux reads
//! **multiple formats**:
//!
//! - **flux-native** — carries explicit `triggers` (substrings that gate activation).
//! - **Agent Skills** (agentskills.io) / **Claude** — `name` + `description`, optional `license`,
//!   `compatibility`, `metadata`, `allowed-tools`; **no `triggers`** (activation is description-led).
//!
//! ```text
//! ---
//! name: rust-style
//! description: How this project writes Rust. Use when editing Rust or running clippy.
//! triggers: [rust, clippy, cargo]   # optional (flux extension)
//! ---
//! <markdown body>
//! ```
//!
//! Parsing is lenient (driven by [`flux_markdown`]); when a skill has no `triggers` we fall back to
//! keywords from its `name`/`description` so foreign skills still activate. Per turn, [`active_for`]
//! selects, ranks, and caps the skills whose activation matches the input — the runtime injects their
//! bodies. Field validation ([`validate`]) is a separate, non-fatal helper for tooling.
//!
//! Discovery is **progressive** (L-02): [`discover`]/[`discover_merged`] read only each file's
//! frontmatter head (Level-1 metadata — `name`, `description`, `triggers`), and a skill's
//! [`SkillBody`] pulls the markdown body from disk the first time it is rendered (Level-2, on
//! selection). Global skill directories therefore cost metadata, not bodies, at startup.

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// Which skill-format family a skill was parsed from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SkillFormat {
    /// flux-native: carries explicit `triggers`.
    #[default]
    Flux,
    /// The cross-agent Agent Skills spec (agentskills.io); also Claude/opencode. Description-driven.
    AgentSkills,
}

/// A discovered skill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skill {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub triggers: Vec<String>,
    #[serde(default)]
    pub body: SkillBody,
    #[serde(default)]
    pub format: SkillFormat,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<PathBuf>,
}

/// A skill body under progressive disclosure: either **inline** text (parsed from memory) or a
/// **lazy** file reference that is read, trimmed, and cached the first time it is needed. Discovery
/// hands out lazy bodies so startup loads Level-1 metadata only; rendering a body (via [`Display`]
/// or [`SkillBody::text`]) is the Level-2 load. A file that has gone unreadable since discovery
/// degrades to an empty body (lenient, like the rest of parsing).
pub struct SkillBody {
    source: BodySource,
    cache: OnceLock<String>,
}

#[derive(Debug, Clone)]
enum BodySource {
    Inline(String),
    File {
        path: PathBuf,
        /// Bytes after the frontmatter block (untrimmed) — a cheap stand-in for the body length so
        /// [`ActivationLimits::max_total_bytes`] can cap without reading anything.
        approx_len: usize,
    },
}

impl SkillBody {
    /// An in-memory body (already loaded).
    pub fn inline(text: impl Into<String>) -> Self {
        SkillBody {
            source: BodySource::Inline(text.into()),
            cache: OnceLock::new(),
        }
    }

    /// A lazy body: read from `path` on first use. `approx_len` is the byte count after the
    /// frontmatter block (used for activation byte-capping without a read).
    pub fn lazy(path: PathBuf, approx_len: usize) -> Self {
        SkillBody {
            source: BodySource::File { path, approx_len },
            cache: OnceLock::new(),
        }
    }

    /// The body text, loading (and caching) it from disk on first use for lazy bodies. An
    /// unreadable file yields `""`.
    pub fn text(&self) -> &str {
        match &self.source {
            BodySource::Inline(s) => s,
            BodySource::File { path, .. } => self.cache.get_or_init(|| load_body(path)),
        }
    }

    /// Whether the body text is resident in memory (inline, or a lazy body already read).
    pub fn is_loaded(&self) -> bool {
        match &self.source {
            BodySource::Inline(_) => true,
            BodySource::File { .. } => self.cache.get().is_some(),
        }
    }

    /// The body length in bytes — exact when loaded, the frontmatter-scan approximation otherwise
    /// (so byte caps never force a read).
    pub fn len(&self) -> usize {
        match &self.source {
            BodySource::Inline(s) => s.len(),
            BodySource::File { approx_len, .. } => {
                self.cache.get().map(String::len).unwrap_or(*approx_len)
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Read a lazy body: re-split the file so trimming matches what [`parse`] would have produced.
fn load_body(path: &Path) -> String {
    match std::fs::read_to_string(path) {
        Ok(content) => {
            let (_, body) = flux_markdown::split_frontmatter(&content);
            body.trim().to_string()
        }
        Err(_) => String::new(),
    }
}

impl Default for SkillBody {
    fn default() -> Self {
        SkillBody::inline(String::new())
    }
}

impl fmt::Display for SkillBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.text())
    }
}

impl fmt::Debug for SkillBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.source {
            BodySource::Inline(s) => f.debug_tuple("SkillBody::Inline").field(s).finish(),
            BodySource::File { path, .. } => f
                .debug_struct("SkillBody::File")
                .field("path", path)
                .field("loaded", &self.is_loaded())
                .finish(),
        }
    }
}

impl Clone for SkillBody {
    fn clone(&self) -> Self {
        let cache = OnceLock::new();
        if let Some(loaded) = self.cache.get() {
            let _ = cache.set(loaded.clone());
        }
        SkillBody {
            source: self.source.clone(),
            cache,
        }
    }
}

impl PartialEq for SkillBody {
    fn eq(&self, other: &Self) -> bool {
        self.text() == other.text()
    }
}
impl Eq for SkillBody {}

impl PartialEq<str> for SkillBody {
    fn eq(&self, other: &str) -> bool {
        self.text() == other
    }
}
impl PartialEq<&str> for SkillBody {
    fn eq(&self, other: &&str) -> bool {
        self.text() == *other
    }
}

impl From<String> for SkillBody {
    fn from(s: String) -> Self {
        SkillBody::inline(s)
    }
}
impl From<&str> for SkillBody {
    fn from(s: &str) -> Self {
        SkillBody::inline(s)
    }
}

impl Serialize for SkillBody {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.text())
    }
}

impl<'de> Deserialize<'de> for SkillBody {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(SkillBody::inline(String::deserialize(d)?))
    }
}

/// How many skills (and how much body) one turn may activate — a guard against prompt bloat now that
/// whole global skill dirs (`~/.claude/skills`, `~/.agents/skills`) are in scope.
#[derive(Debug, Clone, Copy)]
pub struct ActivationLimits {
    pub max_skills: usize,
    pub max_total_bytes: usize,
}

impl Default for ActivationLimits {
    fn default() -> Self {
        Self {
            max_skills: 4,
            max_total_bytes: 24_000,
        }
    }
}

impl Skill {
    /// Does this skill activate for `text`? (Any activation keyword present.)
    pub fn matches(&self, text: &str) -> bool {
        self.match_score(text) > 0
    }

    /// How strongly this skill matches `text` — the count of distinct activation keywords found
    /// (0 = no match). Used to rank skills when several activate.
    pub fn match_score(&self, text: &str) -> usize {
        let lower = text.to_lowercase();
        if !self.triggers.is_empty() {
            // flux-native: substring match (a trigger `rust` fires on `rustup`), preserving prior
            // behavior.
            return self
                .triggers
                .iter()
                .filter(|t| !t.is_empty() && lower.contains(&t.to_lowercase()))
                .count();
        }
        // Fallback for trigger-less (Agent Skills/Claude) skills: whole-word match of keywords from
        // the name + description, so short keys (`pro`, `axon`) don't match inside other words.
        let words: HashSet<&str> = lower
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .collect();
        self.activation_keywords()
            .iter()
            .filter(|k| words.contains(k.as_str()))
            .count()
    }

    /// The keywords that gate fallback activation: hyphen/space-split name parts plus distinctive
    /// (stopword-filtered, length ≥ 4) description tokens.
    fn activation_keywords(&self) -> Vec<String> {
        let mut kws: Vec<String> = Vec::new();
        for part in self
            .name
            .split(|c: char| c == '-' || c == '_' || c.is_whitespace())
        {
            if part.len() >= 3 {
                kws.push(part.to_lowercase());
            }
        }
        for tok in self.description.split(|c: char| !c.is_alphanumeric()) {
            let t = tok.to_lowercase();
            if t.len() >= 4 && !is_stopword(&t) {
                kws.push(t);
            }
        }
        kws.sort();
        kws.dedup();
        kws
    }
}

const STOPWORDS: &[&str] = &[
    "when", "with", "this", "that", "from", "your", "what", "uses", "used", "using", "into",
    "able", "also", "such", "they", "them", "then", "than", "have", "will", "would", "should",
    "could", "about", "which", "where", "while", "their", "there", "these", "those", "other",
    "work", "works", "files", "file", "data", "user", "users", "task", "tasks", "help", "helps",
    "make", "makes", "need", "needs",
];

fn is_stopword(w: &str) -> bool {
    STOPWORDS.contains(&w)
}

/// Frontmatter superset covering flux-native + Agent-Skills/Claude. All fields optional so parsing is
/// lenient; `metadata` keeps raw YAML values so a third-party scalar (number/bool) can't fail it.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SkillFrontmatter {
    name: String,
    description: String,
    #[serde(deserialize_with = "de_triggers")]
    triggers: Vec<String>,
    metadata: BTreeMap<String, serde_norway::Value>,
}

/// Accept `triggers` as a YAML list **or** a comma-separated string.
fn de_triggers<'de, D>(d: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    let raw = match Option::<OneOrMany>::deserialize(d)? {
        None => return Ok(Vec::new()),
        Some(OneOrMany::One(s)) => s.split(',').map(str::to_string).collect(),
        Some(OneOrMany::Many(v)) => v,
    };
    Ok(raw
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect())
}

/// Parse skill content (lenient — malformed frontmatter degrades to a bodyless-frontmatter skill).
pub fn parse(content: &str, source: Option<PathBuf>) -> Skill {
    let (fm, body) = flux_markdown::split_frontmatter(content);
    let meta: SkillFrontmatter = fm
        .map(|y| serde_norway::from_str(y).unwrap_or_default())
        .unwrap_or_default();
    assemble(meta, SkillBody::inline(body.trim().to_string()), source)
}

/// Build a [`Skill`] from parsed frontmatter + a body: name falls back to the source file stem,
/// `metadata.triggers` comma-strings are honored, and the format follows from having triggers.
fn assemble(meta: SkillFrontmatter, body: SkillBody, source: Option<PathBuf>) -> Skill {
    let name = if meta.name.is_empty() {
        source
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "skill".to_string())
    } else {
        meta.name
    };

    // Triggers can also live under `metadata.triggers` as a comma-string (e.g. golang-pro).
    let mut triggers = meta.triggers;
    if triggers.is_empty() {
        if let Some(s) = meta.metadata.get("triggers").and_then(|v| v.as_str()) {
            triggers = s
                .split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect();
        }
    }

    let format = if triggers.is_empty() {
        SkillFormat::AgentSkills
    } else {
        SkillFormat::Flux
    };

    Skill {
        name,
        description: meta.description,
        triggers,
        body,
        format,
        source,
    }
}

/// Parse a skill file progressively: read only the frontmatter head (Level-1 metadata) and hand out
/// a lazy [`SkillBody`] that reads the file when first rendered (Level-2).
fn parse_file(path: &Path) -> Option<Skill> {
    let head = scan_frontmatter(path).ok()?;
    let meta: SkillFrontmatter = head
        .yaml
        .as_deref()
        .map(|y| serde_norway::from_str(y).unwrap_or_default())
        .unwrap_or_default();
    Some(assemble(
        meta,
        SkillBody::lazy(path.to_path_buf(), head.approx_body_len),
        Some(path.to_path_buf()),
    ))
}

/// The result of a frontmatter head scan: the YAML block (if a closed `---` fence was found) plus
/// an approximation of the body's byte length (file length minus the bytes consumed by the fence).
struct FrontmatterHead {
    yaml: Option<String>,
    approx_body_len: usize,
}

/// Stop scanning for a closing fence past this many bytes — a frontmatter block that large is
/// treated as "no frontmatter" (mirroring [`flux_markdown::split_frontmatter`]'s unclosed-fence
/// behavior, where the whole file becomes the body).
const HEAD_SCAN_CAP: usize = 64 * 1024;

/// Read just enough of `path` to capture its frontmatter block, never the body. Mirrors
/// [`flux_markdown::split_frontmatter`]: the file must open with `---` and the block closes at the
/// next line starting with `---` (that line's remainder is discarded); no closing fence means no
/// frontmatter.
fn scan_frontmatter(path: &Path) -> std::io::Result<FrontmatterHead> {
    let file = std::fs::File::open(path)?;
    let file_len = file.metadata()?.len() as usize;
    let mut r = std::io::BufReader::new(file);

    let mut line = String::new();
    r.read_line(&mut line)?;
    let opener = line.trim_start_matches('\u{feff}');
    if !opener.starts_with("---") {
        return Ok(FrontmatterHead {
            yaml: None,
            approx_body_len: file_len,
        });
    }
    // Anything after the opening `---` on the same line belongs to the YAML block.
    let mut yaml = opener[3..].trim_start_matches(['\r', '\n']).to_string();
    let mut consumed = line.len();
    loop {
        line.clear();
        let n = r.read_line(&mut line)?;
        if n == 0 || consumed > HEAD_SCAN_CAP {
            // Unclosed (or absurdly large) fence: lenient, like split_frontmatter — no frontmatter.
            return Ok(FrontmatterHead {
                yaml: None,
                approx_body_len: file_len,
            });
        }
        consumed += n;
        if line.starts_with("---") {
            break;
        }
        yaml.push_str(&line);
    }
    Ok(FrontmatterHead {
        yaml: Some(yaml),
        approx_body_len: file_len.saturating_sub(consumed),
    })
}

/// Discover skills directly under one directory: any `*.md` file, or a subdirectory containing
/// `SKILL.md` (which takes its name from the directory when the frontmatter omits one). Unsorted.
fn discover_dir(dir: &Path) -> Vec<Skill> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let skill_md = path.join("SKILL.md");
            if skill_md.is_file() {
                if let Some(mut s) = parse_file(&skill_md) {
                    // a directory skill takes its name from the directory if unset
                    if s.name == "SKILL" {
                        if let Some(n) = path.file_name() {
                            s.name = n.to_string_lossy().into_owned();
                        }
                    }
                    out.push(s);
                }
            }
        } else if path.extension().map(|e| e == "md").unwrap_or(false) {
            if let Some(s) = parse_file(&path) {
                out.push(s);
            }
        }
    }
    out
}

/// Discover skills under each directory (concatenated, sorted by name; no de-duplication).
pub fn discover(dirs: &[PathBuf]) -> Vec<Skill> {
    let mut out: Vec<Skill> = dirs.iter().flat_map(|d| discover_dir(d)).collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Discover skills across `dirs` **in precedence order** (first wins on a name clash), then sort by
/// name. De-duplication happens before the sort, so precedence is explicit rather than relying on a
/// stable sort.
pub fn discover_merged(dirs: &[PathBuf]) -> Vec<Skill> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for dir in dirs {
        for s in discover_dir(dir) {
            if seen.insert(s.name.clone()) {
                out.push(s);
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The default skill directories, highest-precedence first: the project's `.flux/skills`, then the
/// project's Claude-compatible `.claude/skills`, then the user-global `~/.flux/skills`, then the
/// cross-agent conventions `~/.agents/skills` and `~/.claude/skills`. Canonicalized, de-duplicated,
/// and filtered to existing directories (so the HOME-equals-cwd case can't scan a dir twice). Pass to
/// [`discover_merged`].
pub fn default_skill_dirs(cwd: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    push_default_dirs(&mut dirs, cwd);
    dirs
}

/// Compose the effective skill directories (L-02): `extra` custom dirs first — highest precedence
/// first, e.g. CLI-flag dirs, then project-config dirs, then user-config dirs — layered **above**
/// the built-in well-known set from [`default_skill_dirs`]. Relative extras resolve against `cwd`.
/// Canonicalized, de-duplicated, filtered to existing directories. Pass to [`discover_merged`],
/// where dir order is name-clash precedence.
pub fn skill_dirs(cwd: &Path, extra: &[PathBuf]) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for d in extra {
        let p = if d.is_absolute() {
            d.clone()
        } else {
            cwd.join(d)
        };
        push_existing(&mut dirs, p);
    }
    push_default_dirs(&mut dirs, cwd);
    dirs
}

fn push_default_dirs(dirs: &mut Vec<PathBuf>, cwd: &Path) {
    push_existing(dirs, cwd.join(".flux").join("skills"));
    push_existing(dirs, cwd.join(".claude").join("skills"));
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        push_existing(dirs, home.join(".flux").join("skills"));
        push_existing(dirs, home.join(".agents").join("skills"));
        push_existing(dirs, home.join(".claude").join("skills"));
    }
}

fn push_existing(dirs: &mut Vec<PathBuf>, p: PathBuf) {
    let c = p.canonicalize().unwrap_or(p);
    if c.is_dir() && !dirs.contains(&c) {
        dirs.push(c);
    }
}

/// Select the skills to activate for `input`: those whose activation matches, ranked by match
/// strength (strongest first), capped at `limits.max_skills` and `limits.max_total_bytes` of body
/// (at least one skill is always allowed through, even if it alone exceeds the byte cap).
pub fn active_for<'a>(
    skills: &'a [Skill],
    input: &str,
    limits: ActivationLimits,
) -> Vec<&'a Skill> {
    let mut scored: Vec<(usize, &Skill)> = skills
        .iter()
        .map(|s| (s.match_score(input), s))
        .filter(|(score, _)| *score > 0)
        .collect();
    // Stable sort by score desc keeps original (discovery) order for ties.
    scored.sort_by_key(|b| std::cmp::Reverse(b.0));

    let mut out = Vec::new();
    let mut bytes = 0usize;
    for (_, s) in scored {
        if out.len() >= limits.max_skills {
            break;
        }
        let next = bytes + s.body.len();
        if !out.is_empty() && next > limits.max_total_bytes {
            continue;
        }
        bytes = next;
        out.push(s);
    }
    out
}

/// Validate a skill's `name`/`description` against the Agent Skills naming rules. Returns
/// human-readable issues (empty = valid). **Non-fatal** — discovery never calls this; it's for a
/// future `flux skill lint`.
pub fn validate(skill: &Skill, expected_dir: Option<&str>) -> Vec<String> {
    let mut issues = Vec::new();
    let n = &skill.name;
    if n.is_empty() || n.chars().count() > 64 {
        issues.push(format!(
            "name must be 1-64 characters (got {})",
            n.chars().count()
        ));
    }
    if !n
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        issues.push("name may only contain lowercase letters, digits, and hyphens".into());
    }
    if n.starts_with('-') || n.ends_with('-') {
        issues.push("name must not start or end with a hyphen".into());
    }
    if n.contains("--") {
        issues.push("name must not contain consecutive hyphens".into());
    }
    if let Some(dir) = expected_dir {
        if dir != n {
            issues.push(format!("name `{n}` must match its directory `{dir}`"));
        }
    }
    if skill.description.is_empty() {
        issues.push("description must be non-empty".into());
    }
    if skill.description.chars().count() > 1024 {
        issues.push("description must be at most 1024 characters".into());
    }
    issues
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("flux-skill-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn parses_frontmatter_and_body() {
        let content = "---\nname: rust-style\ndescription: How we write Rust\ntriggers: [rust, clippy]\n---\nUse tabs.\n";
        let s = parse(content, None);
        assert_eq!(s.name, "rust-style");
        assert_eq!(s.description, "How we write Rust");
        assert_eq!(s.triggers, vec!["rust", "clippy"]);
        assert_eq!(s.body, "Use tabs.");
        assert_eq!(s.format, SkillFormat::Flux);
        assert!(s.matches("please run CLIPPY"));
        assert!(!s.matches("python only"));
    }

    #[test]
    fn lenient_without_frontmatter() {
        let s = parse("just a body", Some(PathBuf::from("/x/notes.md")));
        assert_eq!(s.name, "notes");
        assert_eq!(s.body, "just a body");
        assert!(s.triggers.is_empty());
    }

    #[test]
    fn claude_format_without_triggers_activates_via_description() {
        // An Agent-Skills/Claude skill: name + description, no triggers.
        let content = "---\nname: axon\ndescription: Use Axon CLI to index directories and query graph data with AQL\nlicense: MIT\ncompatibility: opencode\n---\nIndex a directory.";
        let s = parse(content, None);
        assert_eq!(s.format, SkillFormat::AgentSkills);
        assert!(s.triggers.is_empty());
        // Activates on a distinctive keyword from name/description...
        assert!(s.matches("can you index this with axon?"));
        assert!(s.matches("query the graph data please"));
        // ...but not on an unrelated prompt.
        assert!(!s.matches("write me a python script"));
    }

    #[test]
    fn nested_metadata_triggers_are_picked_up() {
        // golang-pro stuffs triggers under `metadata:` as a comma string.
        let content = "---\nname: golang-pro\ndescription: Go specialist\nmetadata:\n  version: \"1.0.0\"\n  triggers: Go, Golang, goroutines, gRPC\n---\nbody";
        let s = parse(content, None);
        assert!(s.triggers.contains(&"Golang".to_string()));
        assert!(s.triggers.contains(&"goroutines".to_string()));
        assert_eq!(s.format, SkillFormat::Flux);
        assert!(s.matches("help me with goroutines"));
    }

    #[test]
    fn discovers_md_files_and_skill_dirs() {
        let dir = temp_dir();
        std::fs::write(dir.join("a.md"), "---\nname: alpha\n---\nA").unwrap();
        let sub = dir.join("beta");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(
            sub.join("SKILL.md"),
            "---\ndescription: B skill\n---\nB body",
        )
        .unwrap();

        let skills = discover(std::slice::from_ref(&dir));
        let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"alpha"));
        // directory skill with no `name` takes the directory name
        assert!(names.contains(&"beta"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn discover_merged_precedence_project_wins() {
        let global = temp_dir();
        let project = temp_dir();
        // global-only skill
        std::fs::write(global.join("g.md"), "---\nname: only-global\n---\nG").unwrap();
        // project-only skill
        std::fs::write(project.join("p.md"), "---\nname: only-project\n---\nP").unwrap();
        // collision: both define `shared`
        std::fs::write(global.join("s.md"), "---\nname: shared\n---\nfrom global").unwrap();
        std::fs::write(project.join("s.md"), "---\nname: shared\n---\nfrom project").unwrap();

        // precedence: project first
        let merged = discover_merged(&[project.clone(), global.clone()]);
        let by_name = |n: &str| merged.iter().find(|s| s.name == n).cloned();

        assert!(by_name("only-global").is_some(), "global-only present");
        assert!(by_name("only-project").is_some(), "project-only present");
        let shared = by_name("shared").expect("shared present");
        assert_eq!(shared.body, "from project", "project wins on a name clash");
        // de-duplicated: exactly one `shared`
        assert_eq!(merged.iter().filter(|s| s.name == "shared").count(), 1);

        std::fs::remove_dir_all(&global).ok();
        std::fs::remove_dir_all(&project).ok();
    }

    #[test]
    fn default_dirs_include_project_claude_after_project_flux() {
        let root = temp_dir();
        let flux = root.join(".flux").join("skills");
        let claude = root.join(".claude").join("skills");
        std::fs::create_dir_all(&flux).unwrap();
        std::fs::create_dir_all(&claude).unwrap();
        std::fs::write(flux.join("shared.md"), "---\nname: shared\n---\nfrom flux").unwrap();
        std::fs::write(
            claude.join("shared.md"),
            "---\nname: shared\n---\nfrom claude",
        )
        .unwrap();
        std::fs::write(
            claude.join("claude-only.md"),
            "---\nname: claude-only\n---\nfrom claude",
        )
        .unwrap();

        let dirs = default_skill_dirs(&root);
        assert_eq!(dirs[0], flux.canonicalize().unwrap());
        assert_eq!(dirs[1], claude.canonicalize().unwrap());

        let merged = discover_merged(&dirs);
        let shared = merged.iter().find(|s| s.name == "shared").unwrap();
        assert_eq!(shared.body, "from flux");
        assert!(
            merged.iter().any(|s| s.name == "claude-only"),
            "project .claude/skills should be discovered by default"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn active_for_ranks_and_caps() {
        let skills = vec![
            Skill {
                name: "a".into(),
                description: String::new(),
                triggers: vec!["go".into()],
                body: "x".repeat(100).into(),
                format: SkillFormat::Flux,
                source: None,
            },
            Skill {
                name: "b".into(),
                description: String::new(),
                // two matching triggers → higher score, ranked first
                triggers: vec!["go".into(), "rust".into()],
                body: "y".repeat(100).into(),
                format: SkillFormat::Flux,
                source: None,
            },
            Skill {
                name: "c".into(),
                description: String::new(),
                triggers: vec!["python".into()],
                body: "z".repeat(100).into(),
                format: SkillFormat::Flux,
                source: None,
            },
        ];
        // ranking: b (score 2) before a (score 1); c doesn't match.
        let picked = active_for(&skills, "go and rust please", ActivationLimits::default());
        assert_eq!(
            picked.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["b", "a"]
        );

        // count cap
        let one = active_for(
            &skills,
            "go and rust please",
            ActivationLimits {
                max_skills: 1,
                max_total_bytes: usize::MAX,
            },
        );
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].name, "b");

        // byte cap: first (b, 100 bytes) fits; second (a) would exceed 150 → trimmed.
        let capped = active_for(
            &skills,
            "go and rust please",
            ActivationLimits {
                max_skills: 10,
                max_total_bytes: 150,
            },
        );
        assert_eq!(capped.len(), 1);
    }

    /// Progressive disclosure (L-02): discovery loads Level-1 metadata only; a skill's body is read
    /// from disk on demand. Proven by swapping the file's body *after* discovery — an eager loader
    /// would still hold the original text.
    #[test]
    fn body_is_read_on_demand_not_at_discovery() {
        let dir = temp_dir();
        std::fs::write(
            dir.join("s.md"),
            "---\nname: swap\ndescription: d\n---\nORIGINAL",
        )
        .unwrap();
        let skills = discover(std::slice::from_ref(&dir));
        let s = skills.iter().find(|s| s.name == "swap").unwrap();
        // Swap the body on disk; only a lazy loader sees the new text.
        std::fs::write(
            dir.join("s.md"),
            "---\nname: swap\ndescription: d\n---\nSWAPPED",
        )
        .unwrap();
        assert_eq!(format!("{}", s.body), "SWAPPED");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Selection (`active_for`) ranks and caps on metadata alone; only the *injected* (rendered)
    /// skill's body is ever read. The non-matching skill's body must stay unloaded.
    #[test]
    fn activation_reads_only_the_selected_body() {
        let dir = temp_dir();
        std::fs::write(
            dir.join("a.md"),
            "---\nname: alpha\ntriggers: [qwertyzap]\n---\nALPHA BODY",
        )
        .unwrap();
        std::fs::write(
            dir.join("b.md"),
            "---\nname: beta\ntriggers: [asdfghjkl]\n---\nBETA BODY",
        )
        .unwrap();
        let skills = discover(std::slice::from_ref(&dir));
        assert!(
            skills.iter().all(|s| !s.body.is_loaded()),
            "discovery must not read bodies (Level-1 only)"
        );

        let picked = active_for(
            &skills,
            "please qwertyzap this",
            ActivationLimits::default(),
        );
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].name, "alpha");
        // Selection itself (matching + byte-capping) still reads nothing.
        assert!(skills.iter().all(|s| !s.body.is_loaded()));

        // Injection renders the selected body — the Level-2 read happens here, and only here.
        assert_eq!(picked[0].body.text(), "ALPHA BODY");
        let alpha = skills.iter().find(|s| s.name == "alpha").unwrap();
        let beta = skills.iter().find(|s| s.name == "beta").unwrap();
        assert!(alpha.body.is_loaded());
        assert!(!beta.body.is_loaded(), "unselected body must not be read");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A skill file removed between discovery and injection degrades to an empty body (lenient).
    #[test]
    fn lazy_body_degrades_to_empty_when_file_vanishes() {
        let dir = temp_dir();
        std::fs::write(dir.join("gone.md"), "---\nname: gone\n---\nbody").unwrap();
        let skills = discover(std::slice::from_ref(&dir));
        let s = skills.iter().find(|s| s.name == "gone").unwrap();
        std::fs::remove_file(dir.join("gone.md")).unwrap();
        assert_eq!(s.body.text(), "");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Custom dirs (config/CLI) layer above the defaults: they come first (highest precedence) and
    /// win name clashes in `discover_merged`; relative paths resolve against the cwd.
    #[test]
    fn skill_dirs_layers_extras_above_defaults() {
        let root = temp_dir();
        let default_dir = root.join(".flux").join("skills");
        std::fs::create_dir_all(&default_dir).unwrap();
        std::fs::write(
            default_dir.join("s.md"),
            "---\nname: l02-shared-xyzzy\n---\nfrom default",
        )
        .unwrap();
        let team = root.join("team-skills");
        std::fs::create_dir_all(&team).unwrap();
        std::fs::write(
            team.join("s.md"),
            "---\nname: l02-shared-xyzzy\n---\nfrom extra",
        )
        .unwrap();

        let dirs = skill_dirs(&root, &[PathBuf::from("team-skills")]);
        assert_eq!(dirs[0], team.canonicalize().unwrap(), "extras come first");
        assert_eq!(dirs[1], default_dir.canonicalize().unwrap());

        let merged = discover_merged(&dirs);
        let s = merged
            .iter()
            .find(|s| s.name == "l02-shared-xyzzy")
            .unwrap();
        assert_eq!(s.body, "from extra", "custom dir wins the name clash");

        // A missing extra dir is skipped, not an error.
        let dirs = skill_dirs(&root, &[PathBuf::from("no-such-dir")]);
        assert_eq!(dirs[0], default_dir.canonicalize().unwrap());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn validate_flags_bad_name() {
        let s = parse("---\nname: Bad--Name\ndescription: x\n---\nb", None);
        let issues = validate(&s, None);
        assert!(
            !issues.is_empty(),
            "uppercase + consecutive hyphens should fail"
        );
    }
}
