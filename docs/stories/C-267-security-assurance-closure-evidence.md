---
id: C-267
title: "Record C-186's closure evidence against the 2026-07-29 baseline"
pillar: Core
status: done
priority: 2
epic: security-assurance
design: docs/designs/security-assurance.md
note: "C-186's last unchecked acceptance bullet — every child is done, but nothing records that findings 1-4 and classification trust are closed, so the next review re-derives instead of verifying"
---

# Record C-186's closure evidence against the 2026-07-29 baseline

## Goal

C-186 exists because the 2026-07-29 external review rated flux's security *architecture* 8/10 and its
*assurance* 5/10, and its explicit promise was to "leave a trail that lets the next review verify the
closure instead of re-deriving it." Every child story is now `done`, but that trail was never written:
the epic's last acceptance bullet — a re-run that marks findings 1–4 and classification trust **closed
with evidence**, diffed against the baseline — is still open. Without it the epic's own deliverable is
the one thing it did not deliver, and the next reviewer starts from zero.

## Acceptance

- [x] Each of the 2026-07-29 findings (1–4 plus classification trust) is mapped to the commit, test
      name and file:line that closes it — evidence, not assertion. A finding whose closure cannot be
      pointed at stays **open** and is said to be open. → all five closed with evidence in
      [`reviews/2026-07-30-security-assurance-closure.md`](../../reviews/2026-07-30-security-assurance-closure.md)
      ("Desk-review findings — the closure table"). Both baseline reviews' numbered sets are mapped,
      because "findings 1–4" is genuinely ambiguous between them — the artifact settles the reading
      explicitly instead of picking one silently. **Envelope-integrity finding 4 is reported OPEN**
      (see Progress).
- [x] The mapping is verified against the tree at the current release, not against the story text.
      A child marked `done` whose claimed control is absent or unreachable in the shipped tree is a
      finding in its own right and gets a new story rather than a tick. → every row cites a
      `path:line` read at `0.38.0`/`588144a2`, and the artifact's "The reachability check" section
      records the seven controls whose *production* construction was traced, with the method's limits
      stated. **No done-but-unreachable child was found**, so no new story is owed on that count.
- [x] The result lands as a dated artifact under `reviews/`, in the shape the existing review
      artifacts use, diffed explicitly against the 2026-07-29 baseline so the delta is readable. →
      `reviews/2026-07-30-security-assurance-closure.md`, frontmatter mirroring
      `2026-07-29-security-posture-desk-review.md`, with a per-axis Δ table against that baseline.
- [x] C-186's three stale acceptance boxes are ticked, since C-187/188/189/190, C-191 and
      C-192/193/194 are all `done` — or, if any tick cannot be justified from the tree, it is left
      unticked with the reason recorded. → all three ticked with per-story commit + `file:line`
      evidence; the fourth (the re-run bullet) is ticked by this story, with its reading of
      "findings 1–4" stated and the competing reading's consequence recorded.
- [x] C-205 is recorded as the epic's one **deliberately unclosed** child, with its actual blocker
      stated: `lru 0.12.5` is transitive via `ratatui 0.29.0`, so reaching `>= 0.16.3` requires a
      breaking `ratatui 0.30.x` upgrade, for an *unsound*-class advisory reachable only through
      `LruCache::iter_mut`, which flux never calls. An epic closing over a known-open child must say
      so out loud. → recorded in the epic's Progress and in the artifact. All three legs re-verified:
      `cargo tree -i lru --workspace` → `lru v0.12.5 └── ratatui v0.29.0`; `deny.toml:68-74` states
      the *unsound* class; no `LruCache` exists in `crates/` at all.
- [x] C-186's own `status` is set from what the evidence supports — `done` only if every finding is
      genuinely closed or explicitly and defensibly deferred. → **`in-progress`**, not `done`, on
      three grounds recorded in the epic: envelope finding 4 open with no story, C-266 `ready`, C-205
      `blocked`.
- [x] The board's hand-written Status block is corrected: it currently claims C-186 is "nearly closed
      — C-195 and C-210 remain", and both are `done`. No generator catches that text. → corrected;
      C-195's closure is now named as **won't do** rather than implied to be implemented. The
      generated region was regenerated with `gen_board.py`, not hand-edited.
- [x] Docs coverage tests stay green (`cargo test -p flux-cli --test website_contract`,
      `cargo test -p codewandler-flux-lang --test website_in_sync`). → 18 passed and 3 passed
      respectively.

## Progress

- 2026-07-30 — landed
  [`reviews/2026-07-30-security-assurance-closure.md`](../../reviews/2026-07-30-security-assurance-closure.md),
  verified against the tree at `0.38.0` (`588144a2`). Desk-review findings 1–4 and classification
  trust are **closed with evidence**; envelope-integrity findings 1–3 are closed. Assurance moved
  5/10 → 7.5/10 and is no longer flux's weakest axis (bus factor is). Ledger corrections: C-186's
  four acceptance boxes ticked with evidence, its `status` → `in-progress`, its stale "still open"
  Progress bullet superseded, and the board's hand-written C-186 sentence rewritten.
- **⚠ OPEN, and owed a story this story deliberately did not file: envelope-integrity finding 4.**
  `file_stat` still reads the whole target a second time and discards it —
  `crates/flux-tools/src/extra.rs:96-107`, the discard at `:107`, and the trailing comment's promised
  "note below" still absent from the emitted result (`:108-119`). LOW, and **not** a security defect:
  the guarded read is correct and the author deliberately declined `std::fs::metadata` on the raw path
  to avoid escaping the jail (`:102`) — an instinct any fix should preserve. It survived not by
  decision but because **nobody filed it**: C-192/193/194 map to envelope findings 1–3, and finding 4
  fell off the edge with no story, no "won't do", and no reason recorded.
  **Not filed here on purpose** — allocating a new ID races sibling sessions, story files outside this
  story's own are outside its fence, and the `adversarial-review` skill's boundary is *review, don't
  repair*. Handing it to the coordinator to file.
- **No done-but-unreachable child was found.** This was looked for deliberately, since C-233 and
  C-234 are both prior instances of exactly that pattern in this epic. Seven controls had their
  production construction traced — most usefully, C-190's `guard_open_bind` is reached by the very
  caller the baseline named as the bypass (`crates/flux-channels/src/adapters/a2a.rs:151` →
  `flux_server::router` → `:792`), and C-189's limits come from `ServerLimits::from_env()` at
  `router_with_ttl` (`:822`) on every production mount rather than only through the test injection
  seam. Method limit stated in the artifact: reachability was established by reading call chains and
  CI wiring, **not** by observing a CI run or a live non-loopback bind.
- **Adjacent, not fixed (outside this story's fence):** the board's hand-written `## Status` ends with
  *"**Gate:** green in **both** workspaces"*, and at `588144a2` on a clean tree
  `cargo test --workspace --no-fail-fast` is **red** — 8 tests across 3 `codewandler-flux-lang`
  targets (`--lib`, `--test cst_agreement`, `--test roundtrip_property`), all tracing to `3e2a8b89`
  *refactor(lang): complete the CST parser cutover*. Deterministic, not flaky; 163 other test targets
  pass. Recorded in the artifact under "What this pass found that the ledgers did not".

## Notes

- Read-only over the code; the deliverable is an artifact plus ledger corrections. No behavioural
  change, so no failing-first test — the evidence mapping is the proof obligation instead.
- Three independent adversarial reviews were already run on 2026-07-30 against `cb3bb057` and are
  recorded in `bcfab0ad`; their findings became the C-255 epic, which shipped in 0.38.0. Those are an
  input to this closure, **not** a substitute for it: they targeted a newer tree than the
  2026-07-29 baseline this epic must diff against, and C-255 is a different epic with its own
  outstanding closure bullet.
- The `adversarial-review` skill is at `.agents/skills/adversarial-review/SKILL.md`; existing dated
  artifacts under `reviews/` show the expected shape.
- ⚠ Do not confuse the two epics: C-186 traces to the **2026-07-29** desk + envelope-integrity
  reviews; C-255 traces to the **2026-07-30** three-review round. Closing one does not close the other.
