---
id: C-325
title: "The redaction corpus's synthetic credentials trip GitHub push protection, and it recurs every time the corpus grows"
pillar: Core
status: done
areas: [flux-capabilities, flux-secret]
note: "measured on a real push 2026-07-31: 12 detections across 3 commits and 2 files — C-216's corpus, C-315's additions, and a preservation commit. The literals are provably synthetic (a passing test requires a marker), so each block is a false positive, but every push carrying a new one needs a manual unblock and the old commits stay blocked forever"
---

# The corpus's synthetic credentials trip push protection

## Goal

Stop the redaction corpus from blocking `git push`, permanently, without weakening what it proves.

A corpus that proves the redactor catches a Stripe live key has to contain something shaped like a
Stripe live key. GitHub's push protection sees the shape and blocks the push. Measured on a real
push on 2026-07-31: **12 detections, 3 commits, 2 files, 3 detector rules** (Slack token, Stripe key
under two rules), spanning `crates/flux-capabilities/tests/harness_redaction_corpus.rs` and
`crates/flux-secret/src/lib.rs`.

Every one is a false positive — `every_credential_shaped_literal_in_the_corpus_is_marked_synthetic`
passes, so each corpus literal provably carries a synthetic marker, and the `flux-secret` unit-test
literal self-describes as fake. But the block is real, and **it recurs**: C-216 introduced the first
set, C-315 added more, and each new credential shape the corpus learns to catch adds another.

Two costs, and the second is the one that matters. Each blocked push needs a human to visit an
unblock URL per detector rule. And once a literal is in a commit, **that commit is blocked forever**
— a fresh clone pushing to a fresh remote hits the same wall, and the only escape is rewriting
history, which for a merged story means rewriting the audit trail its review depends on.

## Acceptance

- [x] **No commit contains a literal that matches a secret-scanning rule**, while the corpus still
      asserts over the exact byte sequences it does today. Assembling each literal at run time from
      parts (prefix and body concatenated, or a marker substituted in) is the obvious shape — the
      redactor sees the same bytes, the file on disk does not.
- [x] **Failing-first**: a test or check that fails while a matching literal is present in the source
      and passes once assembly is runtime. This is what stops the next corpus addition reintroducing
      the problem, and without it this story only fixes today's instance.
- [x] The corpus proves *exactly* what it proves now. Re-run the four properties per case —
      redacted, escaped, dropped, deliberately preserved — and confirm no assertion weakened. ⚠ The
      anti-censorship cases matter most here: a preserved case that stops being a literal must still
      be asserted verbatim.
- [x] `every_credential_shaped_literal_in_the_corpus_is_marked_synthetic` still holds, and still
      means something after the change. If literals are assembled, that guard has to check the
      assembled value, not the source fragments — otherwise it passes vacuously, which is the exact
      failure mode C-216 built it to prevent.
- [x] Cover `crates/flux-secret/src/lib.rs`'s own unit-test literals too, not just the corpus. The
      2026-07-31 measurement found two detections there, outside the corpus entirely.
- [x] **State plainly what this does *not* fix**: the already-pushed-blocked commits. Assembling
      literals from here on stops new blocks; it cannot unblock `fd44e0b6`, `17a73b7f` or
      `db5dde82`, which need either a one-time unblock or a history rewrite. Say which the project
      chose and why, so the next person hitting a blocked push knows it is expected.
- [ ] Full gate green in both workspaces. Green except for two things this diff does not own, both
      recorded under Progress: `flux-app`'s `journey_and_direct_flow_produce_the_same_review_report`
      fails identically at the merge base, and `check-crate-versions.sh` owes a version decision on
      the published `codewandler-flux-secret`, which is the integrator's call.

## Notes

- Found by the coordinator on 2026-07-31 attempting to push the 66-commit integration of an
  11-story wave. The push was rejected; nothing was rewritten.
- Related: [C-216](C-216-harness-transcript-redaction-corpus.md) built the corpus and the
  synthetic-marker guard; [C-315](C-315-secret-prefixes-misses-six-credential-shapes.md) added the
  literals that made this recur, and also widened `is_marked_synthetic` to accept a numeric marker
  because an all-digit credential cannot carry an alphabetic one.
- ⚠ **Do not "fix" this by deleting or weakening corpus cases.** The corpus is the only thing
  standing between the redactor and a silent regression, and C-216's whole design point is that a
  redactor which censored everything would fail it. A corpus that no longer contains realistic
  credential shapes proves nothing.
- **The path-allowlist question, answered: it exists, and it is the wrong trade.** GitHub reads
  `.github/secret_scanning.yml` with a `paths-ignore:` glob list (max 1000 entries, max 1 MB), and
  the current docs say the exclusion covers push protection and not only alerting. It is a committed
  file, so unlike a repo *setting* the next clone would inherit it. It was still rejected, for two
  reasons. First, the detections are not confined to test directories: two of them are in
  `crates/flux-secret/src/lib.rs` and four more in `crates/flux-tui/src/`, so the ignore list would
  have to cover production source. Second, and decisive: an allowlist excludes by **path**, not by
  "this literal is synthetic" — a real credential pasted into an excluded file would then never be
  flagged at all, which trades a live detection capability for a cosmetic one. Fragment assembly
  costs one `concat!` per literal, keeps every byte the corpus asserts over, works on any forge, and
  leaves secret scanning fully armed over the whole tree.

## What this does *not* fix

**The already-blocked commits stay blocked.** `fd44e0b6`, `17a73b7f` and `db5dde82` carry the
written-out literals in their trees, and nothing in this story reaches into history. The project
chose the **one-time unblock** that was already granted on 2026-07-31 — three unblock URLs, one per
detector rule — over a history rewrite, because those commits are merged story integrations whose
reviews reference their SHAs, and rewriting them would rewrite the audit trail the reviews depend
on.

The practical consequence, so it is not a surprise: **a fresh clone pushing this repository to a
fresh remote with push protection enabled will be blocked on those three commits**, and will need
the same one-time unblock. That is expected and is not a regression of this story. What this story
guarantees is that no *new* commit joins them — `cargo test -p flux-codegate` fails first, in the
ordinary gate, before the literal can reach a commit at all.

## Progress

**2026-08-01 — implemented.** Every credential-shaped literal in both Cargo workspaces is now joined
from two fragments by `concat!`, with the split always falling **inside the vendor prefix**
(`concat!("sk-ant-", "api03-…")`). `concat!` runs at compile time, so the corpus, the on-disk
fixture and the redactor all receive the byte-identical `&'static str` they received before — no
assertion changed, no case was deleted, and the four properties per case (redacted / escaped /
dropped / preserved) still hold verbatim, including the anti-censorship `preserved` cases.

The recurrence is closed by two guards that fail *first*, in the ordinary gate:

- `flux-codegate`'s `no_workspace_source_carries_a_push_protection_shaped_literal` walks both
  workspaces' `src` and `tests` and fails on any written-out credential shape, naming file, line and
  match. Its table `PUSH_PROTECTION_SHAPES` is set below every real token's length and above the
  registered-value placeholders (`xoxb-redact-me-1234`), and the scanner is proved in both
  directions — it flags a written literal and does *not* flag the same bytes assembled.
- The corpus's own `the_corpus_credentials_are_realistic_but_absent_from_this_file` states both
  halves in one test: the assembled values still carry every vendor shape the corpus claims
  (realism), and none of them appears in the file a forge's scanner reads (absence). Either half
  alone is trivially satisfiable — absence by deleting the credentials, realism by writing them out
  — so they are asserted together.

`every_credential_shaped_literal_in_the_corpus_is_marked_synthetic` reads `case.redacted`, which
holds `concat!(…)` **results**, never their halves. It therefore still checks the assembled value
and cannot pass vacuously; both new guards were mutation-tested (un-split one literal → both fail;
replace one with a non-credential → the realism half fails).

Scope beyond the two files the story names: the same shapes were written out in
`crates/flux-capabilities/tests/harness_history_datasource.rs`, `crates/flux-tui/src/fleet.rs` and
`crates/flux-tui/src/panes.rs`. A repo-wide gate cannot be green with those present, and each is the
same one-line change.

`scripts/check-crate-versions.sh` reports `codewandler-flux-secret changed since v0.43.0 but is
still 1.1.0`. The change there is confined to `#[cfg(test)]` literals — no API, no behaviour — but
the crate is published, so the version decision is the integrator's, not this story's.
