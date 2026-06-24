//! `flux-skill` — markdown knowledge packs with lightweight YAML-ish frontmatter.
//!
//! A skill is a `.md` file (or a directory containing `SKILL.md`) with optional frontmatter:
//!
//! ```text
//! ---
//! name: rust-style
//! description: How this project writes Rust
//! triggers: [rust, clippy, cargo]
//! ---
//! <markdown body>
//! ```
//!
//! Parsing is lenient (missing frontmatter is fine) and dependency-free. Activation/injection is
//! the runtime's concern; this crate just models and discovers skills.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A discovered skill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skill {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub triggers: Vec<String>,
    #[serde(default)]
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<PathBuf>,
}

impl Skill {
    /// Does any trigger appear (case-insensitively) in `text`?
    pub fn matches(&self, text: &str) -> bool {
        let lower = text.to_lowercase();
        self.triggers
            .iter()
            .any(|t| !t.is_empty() && lower.contains(&t.to_lowercase()))
    }
}

fn unquote(s: &str) -> String {
    s.trim().trim_matches(|c| c == '"' || c == '\'').to_string()
}

fn parse_list(v: &str) -> Vec<String> {
    let inner = v.trim().trim_start_matches('[').trim_end_matches(']');
    inner
        .split(',')
        .map(unquote)
        .filter(|p| !p.is_empty())
        .collect()
}

/// Split `---`-delimited frontmatter from the body. Returns `(frontmatter, body)`.
fn split_frontmatter(content: &str) -> (String, String) {
    let t = content.trim_start_matches('\u{feff}');
    if let Some(rest) = t.strip_prefix("---") {
        // rest begins right after the opening `---`; find the closing fence.
        if let Some(end) = rest.find("\n---") {
            let fm = rest[..end].trim_start_matches(['\r', '\n']).to_string();
            let after = &rest[end + 4..]; // skip "\n---"
            let body = after
                .split_once('\n')
                .map(|x| x.1)
                .unwrap_or("")
                .to_string();
            return (fm, body);
        }
    }
    (String::new(), content.to_string())
}

/// Parse skill content (lenient).
pub fn parse(content: &str, source: Option<PathBuf>) -> Skill {
    let (fm, body) = split_frontmatter(content);
    let mut name = String::new();
    let mut description = String::new();
    let mut triggers = Vec::new();
    for line in fm.lines() {
        if let Some((k, v)) = line.split_once(':') {
            match k.trim() {
                "name" => name = unquote(v),
                "description" => description = unquote(v),
                "triggers" => triggers = parse_list(v),
                _ => {}
            }
        }
    }
    if name.is_empty() {
        name = source
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "skill".to_string());
    }
    Skill {
        name,
        description,
        triggers,
        body: body.trim().to_string(),
        source,
    }
}

fn parse_file(path: &Path) -> Option<Skill> {
    let content = std::fs::read_to_string(path).ok()?;
    Some(parse(&content, Some(path.to_path_buf())))
}

/// Discover skills under each directory: any `*.md` file, or a subdirectory containing `SKILL.md`.
pub fn discover(dirs: &[PathBuf]) -> Vec<Skill> {
    let mut out = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
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
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
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
}
