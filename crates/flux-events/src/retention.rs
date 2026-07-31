//! Which ad-hoc stream families the ad-hoc prune may delete — and which it must never touch (C-231).
//!
//! [`EventStore::prune_adhoc_older_than`](crate::EventStore::prune_adhoc_older_than) selects its
//! victims *structurally*: every stream with no `streams` registry row whose newest event predates
//! the cutoff. "Nobody registered this" is the right reading for a D-55 Custom-facts log, and the
//! wrong one for A-107's `memory:<scope-key>` streams — cross-session memory has exactly that shape,
//! so an unguarded sweep would delete it with no error and no signal, and the deleted thing *is* the
//! evidence it would take to reconstruct what was lost.
//!
//! So flux writes the disposition of every ad-hoc stream family it names down here, in
//! [`ADHOC_STREAM_FAMILIES`] — one row per family, each carrying its own reasoning — rather than
//! hiding a `!stream.starts_with("memory:")` inside the prune's query. Three properties follow that
//! the buried predicate would not have:
//!
//! * **A new family decides, rather than inheriting.** [`AdhocRetention`] has no `Default`, so a row
//!   cannot be added without writing `Prunable` or `Retained`, and `why` cannot be left off. The
//!   table is the place the question gets asked out loud.
//! * **Skipping the table is a gate failure, not a silent default.** `every_stream_prefix_declared_in_this_crate_has_a_retention_row`
//!   reads this crate's own source for `STREAM_PREFIX` constants and requires a row for each, so a
//!   future family that never thinks about retention fails the build instead of quietly inheriting
//!   deletion. (Its reach is in-tree families only — see that test's own note.)
//! * **One classifier, every backend.** All three `prune_adhoc_older_than` implementations funnel
//!   their candidate list through [`is_retained_from_adhoc_prune`], so SQLite, Postgres and the
//!   driver-free backend cannot drift apart on which streams are sacred.
//!
//! **Should memory *ever* be prunable?** Answered in writing, deliberately and in one place:
//! `docs/designs/evidence-pinned-memory.md` §7 ("Retention: memory is not a timer's business"). The
//! short form is that unbounded growth is real but tiny here, that a time-based sweep is the wrong
//! instrument for evidence, that `flux memory forget` (A-110) is the deliberate path, and that any
//! future memory retention policy must be scope-aware and opted into explicitly — never inherited
//! from a generic ad-hoc sweep, which is precisely the mistake this table exists to prevent.

use crate::memory::MemoryScope;

/// What the ad-hoc prune may do with one ad-hoc stream family.
///
/// Deliberately without a `Default`: an [`AdhocStreamFamily`] row cannot be written without picking
/// one of these two, which is the point of the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdhocRetention {
    /// Aged streams in this family are deleted by the ad-hoc prune, like any other unregistered
    /// stream. The right answer when the stream is derived, reproducible, or genuinely transient.
    Prunable,
    /// The ad-hoc prune must never delete a stream in this family, at **any** age. The right answer
    /// when the stream is durable state the store is the only copy of, so a time-based sweep would
    /// destroy something no later reader can reconstruct.
    Retained,
}

/// One ad-hoc stream family flux names, and what the ad-hoc prune may do with it.
#[derive(Debug, Clone, Copy)]
pub struct AdhocStreamFamily {
    /// The stream-id prefix that identifies the family (e.g. `"memory:"`).
    pub prefix: &'static str,
    /// Whether the ad-hoc prune may delete an aged stream in this family.
    pub retention: AdhocRetention,
    /// Why — the reasoning a later maintainer needs in order to change this row *honestly*, rather
    /// than because the row was in the way.
    pub why: &'static str,
}

/// Every ad-hoc stream family flux itself names, with its retention decision and the reason for it.
///
/// **Adding an ad-hoc stream family? Add a row here.** The gate makes that mandatory for any family
/// that declares a `STREAM_PREFIX` constant in `flux-events`; for a family named anywhere else, this
/// is still the one place the decision belongs.
///
/// A stream matching no row is `Prunable` by omission, which is correct only because that is what
/// "ad-hoc" means to an embedder's own D-55 log: the store cannot know that a caller's private
/// stream is precious, and D-77 exists exactly to sweep unregistered streams. What must never happen
/// again is *flux's own* durable state landing in that default without anyone noticing.
pub const ADHOC_STREAM_FAMILIES: &[AdhocStreamFamily] = &[
    // A-107 cross-session memory (`memory:<scope-key>`, one stream per scope). Retained: the
    // entries are the agent's evidence-pinned knowledge and this store is their only copy, so the
    // failure mode of pruning them is silent AND unrecoverable. Age is not disuse here either — a
    // memory stream with no event for a year holds knowledge that simply settled, which is the
    // best case for a memory, not a signal that it is unwanted. Forgetting memory is a user verb
    // (`flux memory forget`, A-110) and stays one; see `docs/designs/evidence-pinned-memory.md` §7.
    AdhocStreamFamily {
        prefix: MemoryScope::STREAM_PREFIX,
        retention: AdhocRetention::Retained,
        why: "cross-session memory is durable evidence with no second copy; forgetting it is a \
              user verb (A-110 `flux memory forget`), never a timer's",
    },
];

/// `true` when `stream` belongs to a family the ad-hoc prune must never delete.
///
/// The single classifier behind every backend's `prune_adhoc_older_than`: candidates are filtered
/// through this before anything is deleted, so the three implementations agree by construction.
pub fn is_retained_from_adhoc_prune(stream: &str) -> bool {
    ADHOC_STREAM_FAMILIES.iter().any(|family| {
        matches!(family.retention, AdhocRetention::Retained) && stream.starts_with(family.prefix)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn memory_streams_are_retained_and_ordinary_adhoc_streams_are_not() {
        assert!(is_retained_from_adhoc_prune(&MemoryScope::Global.stream()));
        assert!(is_retained_from_adhoc_prune(
            &MemoryScope::Project {
                key: "flux".to_string()
            }
            .stream()
        ));
        // Not a blanket opt-out: the streams D-77 was written for stay reachable.
        assert!(!is_retained_from_adhoc_prune("audit-2026-07"));
        assert!(!is_retained_from_adhoc_prune("s_42"));
        // Prefix matching, not substring matching — a stream that merely mentions the prefix later
        // in its id is a different family and is not silently protected.
        assert!(!is_retained_from_adhoc_prune("tenant-7/memory:global"));
    }

    /// Every row states a decision and a reason, and no two rows claim the same prefix — the
    /// properties that make the table readable as a decision record rather than a lookup table.
    #[test]
    fn every_family_row_carries_a_distinct_prefix_and_a_reason() {
        for family in ADHOC_STREAM_FAMILIES {
            assert!(
                !family.prefix.is_empty(),
                "an empty prefix would match every stream"
            );
            assert!(
                family.why.len() > 20,
                "family {:?} needs a real reason, not a placeholder",
                family.prefix
            );
        }
        let mut prefixes: Vec<&str> = ADHOC_STREAM_FAMILIES.iter().map(|f| f.prefix).collect();
        prefixes.sort_unstable();
        let count = prefixes.len();
        prefixes.dedup();
        assert_eq!(
            prefixes.len(),
            count,
            "two rows for one prefix means two answers to the same question"
        );
    }

    /// The forcing half of C-231: a stream family declared in this crate cannot reach the ad-hoc
    /// prune without a retention row, so "we never thought about it" fails the gate instead of
    /// inheriting deletion.
    ///
    /// Scans this crate's own source rather than a hand-listed fixture on purpose — a fixture would
    /// only ever agree with the table it was written next to. Its reach is honestly limited: it sees
    /// families that name themselves with a `STREAM_PREFIX` constant in `flux-events`, which covers
    /// how flux names its own (A-107 did exactly that), and it cannot see a prefix an embedder
    /// invents in its own crate. That is the boundary, not an oversight: an embedder's ad-hoc stream
    /// is precisely what D-77 is for.
    #[test]
    fn every_stream_prefix_declared_in_this_crate_has_a_retention_row() {
        let declared = declared_stream_prefixes(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"));
        assert!(
            declared.contains(&(
                "memory.rs".to_string(),
                MemoryScope::STREAM_PREFIX.to_string()
            )),
            "the scan found no `memory:` prefix — it has stopped seeing declarations, \
             so its silence about anything else means nothing (found: {declared:?})"
        );
        for (file, prefix) in &declared {
            assert!(
                ADHOC_STREAM_FAMILIES.iter().any(|f| f.prefix == *prefix),
                "{file} declares the stream prefix {prefix:?} with no row in \
                 ADHOC_STREAM_FAMILIES. Decide: may `prune_adhoc_older_than` delete an aged stream \
                 in that family (`Prunable`) or not (`Retained`)? Add the row and say why."
            );
        }
    }

    /// Every `… STREAM_PREFIX … = "<literal>"` constant under `dir`, as `(file name, literal)`.
    fn declared_stream_prefixes(dir: &Path) -> Vec<(String, String)> {
        let mut found = Vec::new();
        let mut stack: Vec<PathBuf> = vec![dir.to_path_buf()];
        while let Some(path) = stack.pop() {
            let entries = std::fs::read_dir(&path)
                .unwrap_or_else(|e| panic!("read_dir {}: {e}", path.display()));
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default()
                    .to_string();
                let text = std::fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
                for line in text.lines() {
                    let trimmed = line.trim_start();
                    if trimmed.starts_with("//") || !trimmed.contains("const ") {
                        continue;
                    }
                    if !trimmed.contains("STREAM_PREFIX") {
                        continue;
                    }
                    if let Some(literal) = trimmed
                        .split_once('=')
                        .and_then(|(_, rhs)| rhs.split('"').nth(1))
                    {
                        found.push((name.clone(), literal.to_string()));
                    }
                }
            }
        }
        found
    }
}
