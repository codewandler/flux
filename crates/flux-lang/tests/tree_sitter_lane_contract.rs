//! L-118: the nightly tree-sitter lane describes the grammar revision it actually audits.
//!
//! `scripts/check-tree-sitter-corpus.sh` (C-334) resolves the grammar revision **from the pin** in
//! `.helix/languages.toml` and parses every canonical `examples/*.flux` with it. The check itself is
//! therefore self-updating: move the pin and the check moves with it, with nothing to keep in sync.
//!
//! Its *prose* is not. `.github/workflows/tree-sitter-corpus.yml` carries a long preamble stating
//! what the lane is currently measuring and what state that measurement is in — and that preamble is
//! plain text that no `git grep` and no test observed. It went stale exactly once and the failure was
//! instructive: the lane landed red at rev `9ea9890` with a preamble saying so, C-340 fixed the
//! grammar and moved the pin to `2dbec53`, the lane went green — and the preamble went on telling
//! every reader that 7 of 15 canonical examples do not parse. A reader who trusted it would have
//! re-opened work that was already finished, or dismissed a *real* future red as the known standing
//! one. That is the same class as the two failures C-334 exists to catch, one level up: not "the pin
//! does not reflect the mirror" but **"the lane's own contract does not reflect the pin"**.
//!
//! Nightly-only is what let it rot. The lane blocks no push, no PR and no cut (deliberately — see the
//! workflow header), so nothing forces a reader in front of it. This file is the forcing function:
//! it runs in `cargo test --workspace`, on every PR, and it fails the moment the pin moves without
//! the lane's prose moving with it.
//!
//! **Scope, stated plainly.** These are documentation-coherence guards. They assert that the lane
//! names the revision it audits and names owners that exist. They say nothing about whether that
//! revision *parses* anything — only `scripts/check-tree-sitter-corpus.sh` can answer that, it needs
//! the network, and it stays nightly for the reasons its own header records.

use std::collections::BTreeSet;
use std::path::PathBuf;

/// The Helix config that pins the grammar revision. This is the file the corpus check reads, and
/// moving the `rev` in it is the step that actually reaches Helix, Neovim and Zed.
const PIN_FILE: &str = ".helix/languages.toml";

/// The nightly lane whose preamble states what the check measures and what it found.
const LANE_FILE: &str = ".github/workflows/tree-sitter-corpus.yml";

/// Read a repo-root-relative file, or panic naming it.
fn read_repo_file(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The 40-character grammar revision pinned in `.helix/languages.toml`.
///
/// Parsed from the single `source = { git = …, rev = "…" }` line, the same shape
/// `scripts/check-tree-sitter-corpus.sh`'s `read_pin` reads. A pin this function cannot find is a
/// panic rather than a `None`: a guard that silently degrades to "no pin, nothing to check" would
/// pass vacuously in precisely the situation that most needs it.
fn pinned_rev() -> String {
    let src = read_repo_file(PIN_FILE);
    let mut revs = src
        .lines()
        .filter(|line| line.contains("source") && line.contains("git ="))
        .filter_map(|line| line.split_once("rev = \""))
        .filter_map(|(_, rest)| rest.split_once('"'))
        .map(|(rev, _)| rev.to_string());
    let rev = revs.next().unwrap_or_else(|| {
        panic!(
            "{PIN_FILE} has no `source = {{ git = …, rev = \"…\" }}` line — this guard reads that \
             shape and must be taught the new one"
        )
    });
    assert!(
        revs.next().is_none(),
        "{PIN_FILE} pins more than one grammar; the corpus check refuses that too, and this guard \
         cannot tell which revision the lane should name"
    );
    rev
}

/// ⚠ **The nightly lane's preamble must name the grammar revision currently pinned.**
///
/// The one mechanical fact a reader needs from that preamble is *which revision the lane audits*,
/// and it is the one fact that silently expires. Requiring the current pin to appear — in full or in
/// the abbreviated form the preamble uses — costs one line at each pin move and makes the C-340
/// staleness impossible to repeat: a pin move that leaves the prose behind reds here, on a PR, in
/// this workspace, without the network the nightly lane needs.
///
/// It deliberately does **not** forbid *other* revisions appearing. The preamble's history — that
/// the lane was red at `9ea9890`, that `29cff6c` was the pin before it — is worth keeping, and a
/// guard that banned it would push authors to delete the record instead of updating it.
#[test]
fn the_nightly_lane_names_the_grammar_revision_it_audits() {
    let rev = pinned_rev();
    let lane = read_repo_file(LANE_FILE);
    let abbreviated = &rev[..7];
    assert!(
        lane.contains(&rev) || lane.contains(abbreviated),
        "{LANE_FILE} never names the grammar revision {PIN_FILE} pins ({rev}).\n\
         The lane's preamble states what it measures; a pin move that leaves it behind makes it \
         describe a revision nobody audits any more — the state C-340 left behind and L-118 fixed.\n\
         Update the preamble to name {abbreviated} and what it is known to do with the corpus."
    );
}

/// The guard above is only as good as the pin it reads: `contains("")` is true of every file, and a
/// truncated or malformed `rev` would let a stale lane pass while looking rigorous. Pin the shape.
#[test]
fn the_pinned_revision_is_a_full_length_sha() {
    let rev = pinned_rev();
    assert_eq!(
        rev.len(),
        40,
        "{PIN_FILE} pins `{rev}`, which is not a 40-character revision. The corpus check requires \
         the full form (an abbreviated rev cannot be fetched by SHA), and the guards here would \
         weaken to a substring match on a shorter one."
    );
    assert!(
        rev.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "{PIN_FILE} pins `{rev}`, which is not lowercase hexadecimal"
    );
}

/// ⚠ **Every story the lane names must exist**, so a red has an owner a reader can actually open.
///
/// The lane is nightly-only and blocks nothing, so when it goes red the only thing standing between
/// a reader and "ignore it, it is the known one" is the story it points at. A dangling ID — renamed
/// file, typo, a story that was never filed — costs exactly the accountability the reference was
/// there to provide. This is cheap to check and it is the half of L-118's contract that outlives the
/// specific rev.
#[test]
fn the_nightly_lane_names_owner_stories_that_exist() {
    let lane = read_repo_file(LANE_FILE);
    let ids = story_ids(&lane);
    assert!(
        !ids.is_empty(),
        "{LANE_FILE} names no story at all. A nightly lane that blocks nothing needs a named owner \
         in its own header, or a red has nobody to fall to."
    );

    let stories = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/stories");
    let filed: BTreeSet<String> = std::fs::read_dir(&stories)
        .unwrap_or_else(|e| panic!("read_dir {}: {e}", stories.display()))
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().into_string().ok())
        // `C-334-tree-sitter-corpus-check.md` -> `C-334`
        .filter_map(|name| {
            let mut parts = name.splitn(3, '-');
            let pillar = parts.next()?;
            let number = parts.next()?;
            parts.next()?;
            Some(format!("{pillar}-{number}"))
        })
        .collect();

    let dangling: Vec<&String> = ids.iter().filter(|id| !filed.contains(*id)).collect();
    assert!(
        dangling.is_empty(),
        "{LANE_FILE} points at stories with no file in docs/stories/: {dangling:?}\n\
         A nightly red is only actionable if its named owner can be opened."
    );
}

/// The `<PILLAR>-<NUMBER>` story IDs mentioned in a file, deduplicated.
///
/// Matched structurally rather than with a substring search: the lane's prose contains `L-96` and
/// `C-301` inside sentences and inside a URL-ish path, and an ID is any run of an uppercase letter,
/// a hyphen and digits that is not glued to a surrounding word character.
fn story_ids(text: &str) -> BTreeSet<String> {
    let bytes: Vec<char> = text.chars().collect();
    let mut ids = BTreeSet::new();
    for (i, &c) in bytes.iter().enumerate() {
        if !c.is_ascii_uppercase() {
            continue;
        }
        // Not glued to a preceding word character: `SPDX-3` inside an identifier is not a story.
        if i > 0 && (bytes[i - 1].is_alphanumeric() || bytes[i - 1] == '_') {
            continue;
        }
        if bytes.get(i + 1) != Some(&'-') {
            continue;
        }
        let digits: String = bytes[i + 2..]
            .iter()
            .take_while(|d| d.is_ascii_digit())
            .collect();
        if digits.is_empty() {
            continue;
        }
        // And not glued to a following word character either. A trailing hyphen is fine and is
        // wanted: `docs/stories/C-334-tree-sitter-corpus-check.md` names C-334 as surely as prose
        // does, and a lane that references its owner only by path must still count.
        let after = i + 2 + digits.len();
        if bytes
            .get(after)
            .is_some_and(|n| n.is_alphanumeric() || *n == '_')
        {
            continue;
        }
        ids.insert(format!("{c}-{digits}"));
    }
    ids
}

/// `story_ids` is the input to the guard above; an extractor that finds nothing would make it pass
/// vacuously (its emptiness assert would fire, but only for a lane that genuinely names none). Pin
/// the extractor against the shapes the lane actually uses, and against the ones it must not match.
#[test]
fn story_ids_reads_the_shapes_the_lane_uses() {
    let got = story_ids(
        "C-334: the grammar revision pins. The L-96 / `permissions` improvements and C-301's fix \
         arrived. See docs/stories/C-334-tree-sitter-corpus-check.md. Not: SPDX-3, A-, x-12, \
         2026-08-01.",
    );
    let want: BTreeSet<String> = ["C-334", "L-96", "C-301"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(got, want, "story-ID extraction drifted");
}
