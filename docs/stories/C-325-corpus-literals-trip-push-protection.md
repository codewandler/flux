---
id: C-325
title: "The redaction corpus's synthetic credentials trip GitHub push protection, and it recurs every time the corpus grows"
pillar: Core
status: ready
priority: 4
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

- [ ] **No commit contains a literal that matches a secret-scanning rule**, while the corpus still
      asserts over the exact byte sequences it does today. Assembling each literal at run time from
      parts (prefix and body concatenated, or a marker substituted in) is the obvious shape — the
      redactor sees the same bytes, the file on disk does not.
- [ ] **Failing-first**: a test or check that fails while a matching literal is present in the source
      and passes once assembly is runtime. This is what stops the next corpus addition reintroducing
      the problem, and without it this story only fixes today's instance.
- [ ] The corpus proves *exactly* what it proves now. Re-run the four properties per case —
      redacted, escaped, dropped, deliberately preserved — and confirm no assertion weakened. ⚠ The
      anti-censorship cases matter most here: a preserved case that stops being a literal must still
      be asserted verbatim.
- [ ] `every_credential_shaped_literal_in_the_corpus_is_marked_synthetic` still holds, and still
      means something after the change. If literals are assembled, that guard has to check the
      assembled value, not the source fragments — otherwise it passes vacuously, which is the exact
      failure mode C-216 built it to prevent.
- [ ] Cover `crates/flux-secret/src/lib.rs`'s own unit-test literals too, not just the corpus. The
      2026-07-31 measurement found two detections there, outside the corpus entirely.
- [ ] **State plainly what this does *not* fix**: the already-pushed-blocked commits. Assembling
      literals from here on stops new blocks; it cannot unblock `fd44e0b6`, `17a73b7f` or
      `db5dde82`, which need either a one-time unblock or a history rewrite. Say which the project
      chose and why, so the next person hitting a blocked push knows it is expected.
- [ ] Full gate green in both workspaces.

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
- Worth checking whether `.github` supports a push-protection allowlist for specific paths; if it
  does, that may be a cheaper answer than runtime assembly for the test files specifically. Weigh it
  against the fact that an allowlist is a repo setting the next clone does not inherit.
