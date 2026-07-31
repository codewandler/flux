---
id: C-302
title: "The fourth adapter — flux's own session log as HarnessMessage"
pillar: Core
status: ready
priority: 13
epic: harness-history
design: docs/designs/harness-history.md
areas: [flux-capabilities]
note: "split out of C-214, which shipped the three EXTERNAL adapters (codex/claude-code/opencode). flux's own history needs a flux-events dependency in flux-capabilities — legal (L5 → L2) but a manifest+lockfile change, so it runs solo rather than riding a wave"
---

# The fourth adapter — flux's own session log as `HarnessMessage`

## Goal

Give flux's own session history the same `HarnessMessage` projection C-214 built for the three
external harnesses, so `search(query, harness)` in [C-215](C-215-harness-history-datasource.md) can
answer over flux's history and not only over its competitors'.

C-214's Acceptance asked for "all four adapters" and delivered three. The fourth was not skipped for
difficulty: `crates/flux-capabilities/Cargo.toml` has no `flux-events` dependency, and adding one is
a manifest + `Cargo.lock` change, which the wave discipline reserves for a solo story. That is this
story.

## Acceptance

- [ ] `flux_messages` produces `HarnessMessage` from flux's own session log, with `role` mapped
      through the **same** `MessageRole::normalize` the other three adapters use
      (`crates/flux-capabilities/src/harness/message.rs`) — not a parallel mapping. A fourth
      vocabulary resolved a fourth way is the drift this shares a normalizer to prevent.
- [ ] **Failing-first test** `flux_messages_carry_role_and_text`, against a synthetic in-process log
      — no read of the developer's real `~/.flux`, matching C-214's fixture discipline (every one of
      its fixtures is synthetic and inline).
- [ ] **The body budget binds here too, and is proven by a test that drives *this* adapter.** C-214's
      review found the ceiling enforced only on the JSONL paths while every budget test drove
      `claude_messages` alone; the opencode gap that hid behind that is fixed under C-214. Do not
      inherit the same blind spot: assert the total-bytes ceiling, the per-message ceiling, and the
      reported skip counts through `flux_messages`.
- [ ] Malformed or truncated log state is skipped and **counted**, never a panic and never a silent
      truncation. C-214's review flagged a `rows.next()` error that ended a scan while still
      reporting success; whatever this adapter's equivalent is, it must be distinguishable from a
      complete scan.
- [ ] The `flux-events` dependency is added to `crates/flux-capabilities/Cargo.toml` with
      `Cargo.lock` re-locked in the same commit. **L5 → L2 is a legal direction** (`flux-events` is
      L2, `flux-capabilities` L5 in `flux-codegate`'s `layer()` map), so no reclassification is
      needed — but run `cargo test -p flux-codegate` to prove it rather than trusting this sentence.
- [ ] Full gate green in both workspaces.
- [ ] **Drive-by, inherited from C-214's review:** `push_part` in
      `crates/flux-capabilities/src/harness/message.rs` computes
      `cap.saturating_sub(out.len()) + 1`, which **overflows when a caller sets
      `max_message_bytes: usize::MAX`** — a plausible spelling of "no cap" on a `pub` struct with
      `pub` fields. Debug builds panic; release wraps to 0. `saturating_add(1)` closes it. While
      there, correct the adjoining doc: the measured clamp overshoot is **+2 ASCII / +4 for a 3-byte
      char / +5 for a 4-byte char** (the extra byte is the `'\n'` separator when a preceding part
      lands exactly on `cap`), not the "+1 / +4" the comment claims. The clamp itself is correct —
      it rounds *up* to a char boundary deliberately, so a clamped body is always `> cap` and is
      therefore always skipped rather than silently truncated.

## Notes

- **Runs solo.** The manifest and lockfile change is the whole reason this is a separate story; do not
  schedule it alongside another story in the same wave.
- Read C-214's `## Progress` for what the three external adapters settled — the `Pending`/`MessageSink`
  budget machinery, `flatten_content`, and the skip-reason buckets are all reusable and should be
  reused rather than re-derived.
- ⚠ Containment is still owed for the whole family: this extracts, it does not sanitize.
  [C-216](C-216-harness-transcript-redaction-corpus.md) is the containment proof and
  [C-215](C-215-harness-history-datasource.md) is where redaction-at-ingest lands. C-214's review
  confirmed **no in-tree consumer** reaches the extraction API today, which is what keeps the
  unredacted-text exposure theoretical. Adding a consumer before C-215/C-216 would change that —
  don't.
