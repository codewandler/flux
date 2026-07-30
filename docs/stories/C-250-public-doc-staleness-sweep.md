---
id: C-250
title: "Public docs describe a product that has moved — sweep for staleness and keep the enumerations honest"
pillar: Core
status: done
priority: 9
areas: [website]
note: "the published board pages listed seven generated ops when the code shipped nine, within hours of the change — closed enumerations in public docs rot silently because nothing tests them"
---

# Public docs describe a product that has moved — sweep for staleness and keep the enumerations honest

## Goal
Find and fix places where flux's **public** documentation no longer describes the shipped product,
using two angles: docs whose subject code has moved since the doc was last touched, and anything
user-visible added since the last feature release with no public doc at all.

The motivating finding, and the reason this is a recurring hazard rather than a one-off: after C-236
added `board.query` and `board.comments`, the published pages still enumerated a **closed list of
seven** generated board ops when the code generated **nine** — and the two missing ones were exactly
the machine-readable pair a Program consumes as data. A reader following the docs had no way to learn
the capability existed. Nothing failed, because **no test pins those enumerations**; only
`website/docs/language/ops.md` is checked against the live registry, and generated board ops do not
appear in it at all.

## Acceptance
- [x] `website/docs/agent/fleet.md` and `website/docs/agent/datasources.md` enumerate all nine
      generated board ops, and document `query`'s typed rows, the `depends_on` filter and the
      `comments` read-back — with examples verified against the shapes the runtime actually accepts
      (filters nest under `filters`; an `each` source must be a bound value, not a call).
- [x] `website/docs/language/ops.md` states the new selection-op string rule (`regex_extract` single
      match, `first`, `last`, `coalesce` hand back the bare string) **and** its boundary — non-strings
      still come back as JSON, and `split`/`keys`/`all: true` are unaffected.
- [x] `website/docs/language/ops.md` carries a prominent caution for the `git_revert` → `git_reset`
      rename, naming the hazard that the old call *still looks valid* while doing something different.
- [x] The `board.query` row enumeration is complete — it listed eight fields while `item_row` emits
      nine, omitting `attempts`, next to a sentence asserting "every row carries every field".
- [x] Sweep the **remaining** public surface not yet covered: `README.md`, `docs/usage.md`, and the
      rest of `website/docs/**` beyond the board/ops pages.
- [x] Decide whether the closed enumerations that caused this are worth pinning. A doc listing "the
      N generated board ops" cannot be checked by `website_contract` today because generated
      datasource ops never enter the builtin catalog it walks. Either pin them or state why prose that
      cannot be tested is acceptable there — the point is that the choice becomes deliberate.
      **Decision: pinned.** The premise was half right — generated ops really are absent from the
      builtin catalog, but `work_board_tools` and `MemoryBoard` are both public, so a test can
      *generate* the list instead of walking a registry. See the Progress note.
- [x] Standard gate green (the two suites that pin `ops.md` against the registry:
      `flux-cli --test website_contract` 18/18, `flux-tools --test toolspec_invariants` 5/5, plus
      `flux-lang --test website_in_sync` 3/3).

## Progress
- 2026-07-30 — **second pass: the rest of `website/docs/**`, and the enumerations are now pinned.**

  **The pin.** `board_pages_enumerate_every_generated_board_operation_and_query_row_field`
  (`crates/flux-cli/tests/website_contract.rs`) calls `work_board_tools("board", MemoryBoard::new())`
  and reads the op names *and* the `board.query` row fields off `Tool::spec`, then requires
  `fleet.md`/`datasources.md` to name every one. Proved it has teeth the way the blog commit did:
  deleting two ops from `fleet.md` fails naming `board.reassign`/`board.record_evidence`; deleting one
  row field fails naming `attempts` — i.e. it reproduces both historical defects. Row fields are
  asserted on `fleet.md` only, because `datasources.md` links to it instead of keeping a second copy.
  This story's own earlier ticks were **already stale** when this pass started: C-240 took the
  generated ops from nine to eleven. The pages had been updated; nothing had checked them.

  **What the sweep found.** Verified corrections landed across 25 pages. The highest-consequence ones:
  a `[private_net] web_fetch` key documented as "ignored" that actually *refuses to load the config*
  (`deny_unknown_fields`); `opus` documented as `claude-opus-4-8` when it resolves to `claude-opus-5`;
  a `fleet.md` "Current limits" bullet still saying board ops return only text, contradicting the same
  file's own `board.query` section; the board state machine given as four states when seven ship; a
  quoted server-refusal error string that exists nowhere in the tree; MSRV 1.85 when `rust-version` is
  1.87; `AgentEvent` claimed to mirror `AgentSink` "one-to-one" while `tool_timing` has no variant; and
  three `.flux`-is-not-JSON / `$`-sigil-is-mandatory / "exactly one calling convention" claims that
  L-93 falsified hours earlier.

  **Method, so it is repeatable.** Four parallel read-only audits, one per doc area, each required to
  cite a **non-doc** `file:line` for every claimed correction — two docs agreeing was explicitly not
  evidence. Then every finding re-verified here against the tree before editing. That caught one of my
  own edits: I wrote "read the 1h cache tier from the usage endpoints instead", then found those
  endpoints share the same `usage_json` and omit it too.

  **Deliberately not done.** L-93 made the compact form canonical and left the whole example corpus in
  the older `$`-sigil spelling. Those snippets still parse (`complete_flux_fences…` bans only `let`),
  so they are not *false*; migrating the corpus is a separate story. The pages that *claimed* the old
  spelling was mandatory or canonical were corrected, and `tour.md`'s "Every snippet uses current
  syntax" was softened to what is true. Also left: the four `flux plugin status` sample outputs, whose
  pack version (0.1.4 now) and `op(s)` counts are both stale — the counts include a host-injected
  `plugin.validate`, so getting them right needs the plugin binaries built and run, not read.

  Gate: `cargo test --workspace` 167 suites all green, `cargo clippy --workspace --all-targets
  -D warnings` clean, `cargo fmt --all --check` clean, `cargo test -p flux-codegate` green,
  `website_contract` 19/19, `website_in_sync` 3/3, and `npm run build` exit 0 with
  `onBrokenLinks`/`onBrokenAnchors` at `throw` — which is what proves the two anchors this pass added
  resolve.
- 2026-07-30 — **first pass merged.** Left `ready` rather than `done`: the four ticked items shipped,
  the remaining public surface (`README.md`, `docs/usage.md`, the rest of `website/docs/**`) and the
  decision about pinning enumerations are still open.
  Verified at integration: `flux-cli --test website_contract` 18/18,
  `codewandler-flux-tools --test toolspec_invariants` 5/5,
  `codewandler-flux-lang --test website_in_sync` 3/3. A full workspace gate was **not** re-run for this
  merge and did not need to be — the diff is markdown under `website/docs/` only, so build, clippy, fmt
  and codegate results carry over unchanged from C-230's green gate on identical code; what a docs
  change can break is the suites that read those files at runtime, and those are the three above.
  No `WHATS-NEW.md` entry: the capabilities themselves were already announced there under C-236 and
  C-238, and this is the reference documentation catching up, not a product change.
- 2026-07-30 — first pass landed on `docs/staleness-sweep-0.36` (three commits). The sweep agent was
  stopped part-way; its work was preserved and completed by the coordinator, which is how the
  `attempts` omission was caught — **the sweep's own output had the same defect class it was opened to
  fix**, an enumeration the code had outgrown. Worth remembering as a review rule: a doc fix that
  lists fields or ops needs the same check against the code that the original doc failed.
- 2026-07-30 — four claims verified mechanically against the code rather than by eye:
  `DependencyMatch::ALL` is `[Satisfied, Unsatisfied]`; exactly nine board ops are generated; `filters`
  nests under `additionalProperties: false`; and `item_row` emits every field, so unset optionals
  serialise as `null`.

## Notes
- **Why this recurs:** the public site has real contract tests, but they cover only the *builtin* op
  catalog and the generated prelude/node-kind blocks. Hand-written prose that happens to enumerate
  something — ops, fields, states — is unguarded, and enumerations are exactly what goes stale when a
  capability is added. That asymmetry is the durable finding, not any individual wrong list.
- `WHATS-NEW.md` and `website/docs/whats-new.md` are out of scope here: the mirror is generated and
  pinned by `website_customer_changelog_is_in_sync`, and the customer entries for this cycle
  (including the `git_reset` rename under "Action needed") are already written.
- Voice rules for anything public are codified in the HTML comment at the top of `WHATS-NEW.md`:
  plain language, feature-first, no story IDs, no crate names.
