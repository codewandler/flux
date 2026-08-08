//! C-744 — the delivery glossary is held to the code it describes, and stories are held to it.
//!
//! `docs/glossary.md` defines the vocabulary the board and fleet contracts depend on. A glossary
//! that is merely written becomes archaeology: it lags the first rename and then quietly teaches the
//! wrong word. Two checks keep it current, and both are deliberately structural rather than
//! stylistic — a lint people suppress is worse than no lint.
//!
//! 1. **Every entry anchors to something real.** A `board`/`fleet` anchor must still be an operation
//!    the binary offers; any other anchor must still appear in the board/fleet command source. Rename
//!    a verb or a field and this fails until the glossary changes in the same commit.
//! 2. **No story renames a defined concept.** The "words that are not our words" table lists exact
//!    drift phrasings — `block the wave` for a park, `lock the story` for a claim — and no planning
//!    document may use one. The phrases are collocations rather than bare words on purpose: `batch`,
//!    `block` and `lock` all have honest uses here, and flagging those would train everyone to
//!    ignore the check.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/flux-cli has a workspace root")
        .to_path_buf()
}

fn read(relative: &str) -> String {
    let path = repo_root().join(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

/// Every backticked token on a line, in order.
fn backticked(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut rest = line;
    while let Some(open) = rest.find('`') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('`') else { break };
        tokens.push(after[..close].to_string());
        rest = &after[close + 1..];
    }
    tokens
}

/// `### term` → the entry's `- Anchor:` tokens, plus whether it carries a `- Not:` distinction.
fn glossary_entries(glossary: &str) -> BTreeMap<String, (Vec<String>, bool)> {
    let mut entries = BTreeMap::new();
    let mut term = None;
    for line in glossary.lines() {
        if let Some(heading) = line.strip_prefix("### ") {
            term = Some(heading.trim().to_string());
            entries.insert(heading.trim().to_string(), (Vec::new(), false));
            continue;
        }
        let Some(term) = term.as_deref() else {
            continue;
        };
        let entry: &mut (Vec<String>, bool) = entries.get_mut(term).expect("term was inserted");
        if let Some(anchor) = line.trim().strip_prefix("- Anchor:") {
            entry.0.extend(backticked(anchor));
        }
        if line.trim().starts_with("- Not:") {
            entry.1 = true;
        }
    }
    entries
}

/// The first column of the "words that are not our words" table: the phrasings a story may not use.
fn forbidden_phrases(glossary: &str) -> Vec<(String, String)> {
    let mut phrases = Vec::new();
    let mut in_table = false;
    for line in glossary.lines() {
        if line.starts_with("| Do not write") {
            in_table = true;
            continue;
        }
        if in_table && !line.starts_with('|') {
            break;
        }
        if !in_table || line.starts_with("|---") {
            continue;
        }
        let mut columns = line.trim_matches('|').split('|');
        let (Some(banned), Some(instead)) = (columns.next(), columns.next()) else {
            continue;
        };
        for phrase in backticked(banned) {
            phrases.push((phrase, instead.trim().to_string()));
        }
    }
    phrases
}

/// Whether `phrase` occurs in `line` as whole words.
///
/// Substring matching is not good enough for a lint nobody is allowed to suppress: `block the story`
/// contains `lock the story`, and one such finding teaches everyone to stop reading the output.
fn contains_phrase(line: &str, phrase: &str) -> bool {
    let line = line.to_ascii_lowercase();
    let phrase = phrase.to_ascii_lowercase();
    let boundary = |character: Option<char>| {
        character.is_none_or(|character| !character.is_alphanumeric() && character != '_')
    };
    let mut offset = 0;
    while let Some(found) = line[offset..].find(&phrase) {
        let start = offset + found;
        let end = start + phrase.len();
        if boundary(line[..start].chars().next_back()) && boundary(line[end..].chars().next()) {
            return true;
        }
        offset = start + 1;
    }
    false
}

/// The operation names one command family actually offers, straight from its own schema.
fn operations(family: &str) -> Vec<String> {
    let root = std::env::temp_dir().join(format!(
        "flux-delivery-vocabulary-{family}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("docs/stories")).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_flux"))
        .current_dir(&root)
        .env("FLUX_SANDBOX", "off")
        .args([family, "schema", "--output", "json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "flux {family} schema failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let schema: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let names = schema["data"]["operations"]
        .as_array()
        .unwrap_or_else(|| panic!("flux {family} schema has no operations: {schema}"))
        .iter()
        .map(|operation| operation["name"].as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>();
    fs::remove_dir_all(&root).ok();
    names
}

/// C-744, failing first: a glossary that cannot lag the code it describes.
#[test]
fn every_glossary_anchor_still_resolves() {
    let glossary = read("docs/glossary.md");
    let source = read("crates/flux-cli/src/board_fleet_cmd.rs");
    let board = operations("board");
    let fleet = operations("fleet");
    let entries = glossary_entries(&glossary);
    assert!(
        entries.len() >= 40,
        "the glossary defines the vocabulary the contracts depend on, not a handful of it: {} entries",
        entries.len()
    );

    for (term, (anchors, has_distinction)) in &entries {
        assert!(
            anchors.len() == 1,
            "`{term}` needs exactly one `- Anchor:` line naming what it describes; found {anchors:?}"
        );
        assert!(
            *has_distinction,
            "`{term}` needs a `- Not:` line — an entry without the distinction that makes the term \
             non-obvious is a dictionary entry, and the contracts already have those"
        );
        let anchor = &anchors[0];
        match anchor.split_once(' ') {
            Some(("board", operation)) => assert!(
                board.iter().any(|name| name == operation),
                "`{term}` anchors to `flux board {operation}`, which is not an operation this binary \
                 offers — rename the glossary in the same commit as the verb"
            ),
            Some(("fleet", operation)) => assert!(
                fleet.iter().any(|name| name == operation),
                "`{term}` anchors to `flux fleet {operation}`, which is not an operation this binary \
                 offers — rename the glossary in the same commit as the verb"
            ),
            _ => assert!(
                source.contains(anchor.as_str()),
                "`{term}` anchors to `{anchor}`, which no longer appears in \
                 crates/flux-cli/src/board_fleet_cmd.rs — rename the glossary in the same commit as \
                 the code"
            ),
        }
    }
}

/// C-744, failing first: a story that renames a concept is how the vocabulary drifts.
#[test]
fn no_story_renames_a_defined_concept() {
    let glossary = read("docs/glossary.md");
    let phrases = forbidden_phrases(&glossary);
    assert!(
        phrases.len() >= 8,
        "the near-miss table is what makes the glossary enforceable: {phrases:?}"
    );

    let stories = repo_root().join("docs/stories");
    let mut findings = Vec::new();
    for entry in fs::read_dir(&stories).expect("docs/stories").flatten() {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("md") {
            continue;
        }
        let Ok(body) = fs::read_to_string(&path) else {
            continue;
        };
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        for (line_number, line) in body.lines().enumerate() {
            for (phrase, instead) in &phrases {
                if contains_phrase(line, phrase) {
                    findings.push(format!(
                        "{name}:{}: \"{phrase}\" — say {instead}",
                        line_number + 1
                    ));
                }
            }
        }
    }
    assert!(
        findings.is_empty(),
        "these planning documents rename a concept the glossary defines:\n{}",
        findings.join("\n")
    );
}

/// C-744: the glossary is part of what an agent reads before acting, not a document it could skip.
#[test]
fn the_repository_contract_requires_the_glossary() {
    let agents = read("AGENTS.md");
    assert!(
        agents.contains("docs/glossary.md"),
        "AGENTS.md is what every agent reads first; a glossary it does not name is one no agent reads"
    );
}
