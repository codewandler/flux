//! `flux changelog` — the customer-facing "what has changed" view (C-48).
//!
//! The content is the repo-root `WHATS-NEW.md` (plain-language, per-release sections),
//! embedded at build time — flux-cli is a binary-only crate built from the repo, so the
//! repo-root `include_str!` is safe (same pattern as flux-app's embedded examples). By
//! default the running binary's own version section is shown; when that version has no
//! section (a release with no user-visible changes), the most recent non-empty release
//! is shown with a note.

use anyhow::{bail, Result};

/// The embedded customer changelog (rolled by `scripts/cut-release.sh` on every cut).
const WHATS_NEW: &str = include_str!("../../../WHATS-NEW.md");

/// One `## [version]` section of the file: the heading label and its markdown body.
struct Section {
    /// `Unreleased`, `0.11.6`, or a range label like `0.9 – 0.11.3`.
    label: String,
    /// The heading line + everything up to the next `## [` heading (markdown).
    source: String,
}

/// Split a WHATS-NEW-style document into its `## [label]` sections, in file order
/// (newest first, by convention). Content before the first section (the title and the
/// voice-rules comment) is dropped.
fn split_sections(src: &str) -> Vec<Section> {
    let mut sections: Vec<Section> = Vec::new();
    for line in src.lines() {
        if let Some(rest) = line.strip_prefix("## [") {
            if let Some(end) = rest.find(']') {
                sections.push(Section {
                    label: rest[..end].to_string(),
                    source: String::new(),
                });
            }
        }
        if let Some(current) = sections.last_mut() {
            current.source.push_str(line);
            current.source.push('\n');
        }
    }
    sections
}

/// Whether a section has any content beyond its heading (blank lines don't count).
fn has_content(s: &Section) -> bool {
    s.source
        .lines()
        .skip(1)
        .any(|l| !l.trim().is_empty() && !l.trim_start().starts_with("<!--"))
}

/// Render one or more sections to the terminal via the shared markdown renderer.
fn render(sections: &[&Section]) -> Result<()> {
    let width = crossterm::terminal::size()
        .map(|(w, _)| (w as usize).clamp(40, 100))
        .unwrap_or(100);
    let theme = flux_markdown::render::Theme::auto();
    for s in sections {
        print!(
            "{}",
            flux_markdown::render::render_ansi(&s.source, &theme, width)
        );
        println!();
    }
    Ok(())
}

/// `flux changelog [<version>] [--all] [--unreleased]`.
pub fn run(version: Option<&str>, all: bool, unreleased: bool) -> Result<()> {
    let sections = split_sections(WHATS_NEW);
    if sections.is_empty() {
        bail!("the embedded WHATS-NEW.md has no release sections — this is a build defect");
    }

    if all {
        let all_refs: Vec<&Section> = sections.iter().collect();
        return render(&all_refs);
    }
    if unreleased {
        let Some(s) = sections.iter().find(|s| s.label == "Unreleased") else {
            bail!("no [Unreleased] section in the embedded WHATS-NEW.md");
        };
        return render(&[s]);
    }
    if let Some(v) = version {
        let v = v.trim_start_matches('v');
        let Some(s) = sections.iter().find(|s| s.label == v) else {
            let known: Vec<&str> = sections.iter().map(|s| s.label.as_str()).collect();
            bail!(
                "no \"what's new\" section for version {v} — known sections: {}",
                known.join(", ")
            );
        };
        return render(&[s]);
    }

    // Default: this binary's version, falling back to the newest non-empty release.
    let own = env!("CARGO_PKG_VERSION");
    if let Some(s) = sections
        .iter()
        .find(|s| s.label == own)
        .filter(|s| has_content(s))
    {
        return render(&[s]);
    }
    let Some(latest) = sections
        .iter()
        .find(|s| s.label != "Unreleased" && has_content(s))
    else {
        bail!("no non-empty release section in the embedded WHATS-NEW.md");
    };
    println!(
        "(no user-visible changes recorded for {own}; showing the most recent release with \
         highlights)\n"
    );
    render(&[latest])
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = "\
# What's new

## [Unreleased]

### New

- upcoming thing

## [1.2.3] - 2026-01-02

### Improved

- better thing

## [1.2.2] - 2026-01-01
";

    #[test]
    fn splits_sections_in_file_order() {
        let s = split_sections(FIXTURE);
        let labels: Vec<&str> = s.iter().map(|x| x.label.as_str()).collect();
        assert_eq!(labels, vec!["Unreleased", "1.2.3", "1.2.2"]);
        assert!(s[1].source.contains("better thing"));
        assert!(s[1].source.starts_with("## [1.2.3]"));
    }

    #[test]
    fn empty_release_sections_are_detected() {
        let s = split_sections(FIXTURE);
        assert!(has_content(&s[0]) && has_content(&s[1]));
        assert!(!has_content(&s[2]), "1.2.2 has a heading but no content");
    }

    #[test]
    fn the_real_embedded_file_parses_with_release_sections() {
        let s = split_sections(WHATS_NEW);
        assert!(
            s.iter().any(|x| x.label == "Unreleased"),
            "WHATS-NEW.md must keep an [Unreleased] section (the release script rolls it)"
        );
        assert!(
            s.iter().filter(|x| x.label != "Unreleased").count() >= 1,
            "expected at least one release section"
        );
        assert!(
            s.iter()
                .filter(|x| x.label != "Unreleased")
                .any(has_content),
            "expected a non-empty release section for the default fallback"
        );
    }
}
