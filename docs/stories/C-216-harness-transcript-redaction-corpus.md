---
id: C-216
title: "Prove the containment — a redaction corpus over real transcript shapes, and an opt-out audit"
pillar: Core
status: in-progress
priority: 13
epic: harness-history
design: docs/designs/harness-history.md
note: "C-215 establishes the envelope on the happy path; this makes it hold under the shapes transcripts actually take — base64 blobs, multi-part content, tool_result payloads, and text the redactor's prefix list has never seen"
---

# Prove the containment — a redaction corpus over real transcript shapes, and an opt-out audit

## Goal
C-215 lands the containment envelope: opt-in, escaping, ingest-time redaction, per-harness subjects.
This story establishes that it holds against what transcripts *actually contain*, rather than against
the one fixture that proved the mechanism was wired.

The redactor is a lossy heuristic by design — a fixed prefix list plus registered values matched by
substring, with a 6-character registration floor. That is an honest trade on a log line. On a corpus
of years of conversation it under-matches in ways worth measuring before the feature ships, not
after.

## Acceptance
- [ ] A transcript **corpus fixture** covering the shapes these logs really take, per harness: a
      multi-part content array, a `tool_result` carrying command output, a base64 blob, an env-dump
      paste, a heredoc'd config, and a message whose text is itself instruction-shaped. Committed as
      fixtures, never as real user data.
- [ ] Each corpus case asserts the specific containment property it targets — redacted, escaped, or
      both — so a regression names which property broke rather than just failing.
- [ ] **The under-match is measured and written down**, not assumed away: state in the design doc
      which credential shapes the redactor does *not* catch in this corpus, and what the operator's
      recourse is (`add_secret` registration, or leaving the datasource off). A known, documented gap
      is a decision; an unmeasured one is a claim.
- [ ] **The opt-out audit**: a test that walks every candidate root for all four harnesses with the
      datasource disabled and asserts not one is opened — extending C-215's single-path check to
      every discovery branch, including the env-override paths, since those are the ones a test that
      only sets `HOME` never reaches.
- [ ] Re-scan idempotence: ingesting the corpus twice produces the same record set with the same ids,
      so the index does not silently accumulate duplicates as sessions grow.
- [ ] Standard gate green in both workspaces.

## Progress
- 2026-07-29 — filed with the epic. Depends on C-215; do not start before it lands.

## Notes
- Seams: the redactor is `flux-secret` (`add_secret`, the prefix list, the 6-character floor); the
  escaping precedent is A-21's `<knowledge-base>` body escaping.
- **Do not "fix" the redactor's heuristics inside this story.** If the corpus shows a gap worth
  closing in `flux-secret` itself, that is a separate story with its own blast radius — the redactor
  is shared by the stream-json writer, the whatif cassette and the evidence flush, and widening its
  matching affects all of them.
- **Fixtures are synthetic.** Never commit a real transcript, and never generate the corpus by
  scraping the developer's own `~/.claude` or `~/.codex`. Hand-author the shapes.
- The opt-out audit is the story's highest-value item. "Off by default" is the whole basis on which
  this epic is safe to ship, and it is exactly the kind of property that holds on the path someone
  tested and quietly fails on the branch nobody did.
