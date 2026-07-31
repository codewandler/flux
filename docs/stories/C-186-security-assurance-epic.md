---
id: C-186
title: "Security assurance — close the gap between the envelope and its proof (epic)"
pillar: Core
status: in-progress
priority: 1
epic: security-assurance
design: docs/designs/security-assurance.md
note: "REVIEW EPIC — every child traces to a CONFIRMED finding in one of the two 2026-07-29 adversarial reviews (desk review + envelope-integrity); architecture rated 8/10 while assurance rated 5/10, and the spread is the work"
---

# Security assurance — close the gap between the envelope and its proof (epic)

## Goal
The 2026-07-29 external adversarial review rated flux's security *architecture* 8/10 and its
security *assurance* 5/10. That spread is this epic. flux claims a non-bypassable authorization →
approval → guarded-IO envelope; today almost nothing outside the envelope proves it stays that way,
and the supply chain that delivers flux to a user is the softest part of the system. Close the
confirmed, actionable half of that gap — and leave a trail that lets the next review verify the
closure instead of re-deriving it.

## Acceptance
- [x] C-187 (SHA-pin actions), C-188 (advisory scanning), C-189 (server limits) and C-190
      (construction-time auth invariant) are done, each with the failing-first test or failing-CI
      demonstration its story names. → verified against the tree at `0.38.0`, not against the story
      text: C-187 `efac6efd` (zero unpinned `uses:` in all 9 workflows; guard at `ci.yml:137-139`),
      C-188 `e3d67e39` (`security-audit.yml`, push/PR/weekly), C-189 `c9c5086e`
      (`flux-server/src/lib.rs:913`+`:1120`, tests `:2136`/`:2167`), C-190 `8e322973`
      (`guard_open_bind` `:512` called from `router` `:792`, test `:2687`). Evidence table:
      [`reviews/single/2026-07-30-security-assurance-closure.md`](../reviews/single/2026-07-30-security-assurance-closure.md).
- [x] C-191 lands a registry-wide `ToolSpec` invariant test, converting the review's
      "classification trust" concern from an assumption into a gate. → `d25eeab6`;
      `flux-spec/src/coherence.rs` I1/I2/I3 (`:256`/`:268`/`:280`) now gated at **six** seams, four
      of them added by later passes that caught the gate narrower than it claimed (C-208, C-210,
      C-233, C-234). The census raised 22 violations across 19 ops the first time it ran.
- [x] C-192 (the `sqlite_query` guarded-IO bypass), C-193 (statement allowlist) and C-194 (the
      mechanical no-direct-IO lint) are done. These trace to the **envelope-integrity** review, not
      the desk review, and they matter disproportionately: C-192 is the epic's only *confirmed
      bypass* of the envelope rather than a missing assurance step, and C-194 is the check that
      would have caught it at authoring time. → C-192/C-193 `5031bd30` (statement allowlist at
      `flux-tools/src/extra.rs:223`/`:384-392`; `VACUUM` refused *as a statement type*, tests at
      `:732`/`:765`/`:797`/`:828`); C-194 `0c529310`+`5b253e6a`, hardened into a `syn` scanner by
      C-263, gated at `ci.yml:155-157`.
- [x] A re-run of the [`adversarial-review`](../../.agents/skills/adversarial-review/SKILL.md) skill
      against the then-current version can mark findings 1–4 and classification trust **closed with
      evidence**, diffed against the 2026-07-29 baseline. →
      [`reviews/single/2026-07-30-security-assurance-closure.md`](../reviews/single/2026-07-30-security-assurance-closure.md)
      (C-267). ⚠ "findings 1–4" is ambiguous between the two baseline reviews; the artifact settles
      it explicitly as **this design doc's own risk ranking** (items 1–4 = C-187/188/189/190,
      item 5 = classification trust), which is the reading the three bullets above enumerate and the
      only one under which the phrase is satisfiable. Under the *other* reading — the
      envelope-integrity review's numbered findings — this bullet would **not** be tickable, because
      its finding 4 is open (see Progress).
- [x] The deferred sandbox-default question (see Notes) has either become its own story or been
      consciously dropped with the reason recorded. → **C-217** files step 1 (make `on` report its
      resolved posture); step 2, the default flip itself, stays deferred behind it by design.

## Progress

- 2026-07-30 — **envelope-integrity finding 4 is now filed as
  [C-275](C-275-file-stat-discards-a-whole-read.md)**, closing the one gap C-267 could not close itself.
  It had survived by never being filed rather than by decision: C-192/193/194 map to envelope findings
  1–3, and the fourth fell off the edge with no story and no "won't do". Verified in the tree before
  filing (`crates/flux-tools/src/extra.rs:96-107` reads the whole file into `.map(|_| …)` and discards
  it; the "note below" its comment promises is absent from both the JSON and the view).
  This epic therefore now waits on exactly two things: **C-275** and **C-205** (`blocked` on the
  `ratatui` 0.29 hold, for an *unsound*-class advisory flux cannot reach). Both are recorded rather
  than implied, so the epic can be closed by evidence rather than by fatigue.
- 2026-07-29 — epic opened from the review. Design:
  [security-assurance.md](../designs/security-assurance.md). Source review:
  [`reviews/single/2026-07-29-security-posture-desk-review.md`](../reviews/single/2026-07-29-security-posture-desk-review.md),
  verified claim-by-claim against the tree at `0.33.1` — every child story cites a `path:line`, not
  the reviewer's prose.
- Ordering is **not** the review's ordering. Ranked by risk × reachability ÷ cost, which puts the
  supply-chain item first and the review's own headline finding out of scope (see Notes).
- 2026-07-29 — second review, `envelope-integrity` lens:
  [`reviews/single/2026-07-29-envelope-integrity.md`](../reviews/single/2026-07-29-envelope-integrity.md).
  Added C-192, C-193, C-194. C-192 inserted at priority 2 — ahead of advisory scanning and the
  server limits — because it is model-reachable in any default session with no operator mistake and
  no third party required; C-188/C-189/C-190 shifted to 5/6/7. That pass **confirmed** the dispatch
  chain itself is sound on every path examined (shared `gate` between `dispatch` and a synchronous
  `authorize`, cap-scope checked before hooks, filesystem subjects normalized to physical identity,
  no production `Tool::execute` call, workspace root not model-reachable) — the failure came from
  outside the envelope, which is the argument for C-194 over more envelope hardening.
- 2026-07-29 — **eight of the epic's children landed** in two impl-coord waves, each merged only
  after its gate ran green and (for every envelope-touching change) an independent fresh-context
  review passed: **C-187** (SHA-pin actions + a CI pin guard), **C-192**/**C-193** (the confirmed
  `VACUUM INTO` guarded-IO bypass closed by a statement allowlist; review SOUND on 36k+ fuzzed
  inputs), **C-189** (daemon body limits + timeouts; review SOUND incl. timeout-cancellation
  leaving a valid session log), **C-188** (cargo-audit + cargo-deny advisory scanning; the real
  gate verified locally), **C-190** (unauthenticated-non-loopback refusal now holds at router
  construction, breaking; review SOUND, also closing the C-189 real-router auth-test gap), and
  **C-194** (the mechanical no-direct-IO lint; first cut was review-caught as bypassable in the
  unsafe direction, reworked into a string/comment-aware tokenizer, re-verified against a novel
  bypass). Two adjacent items were split out as their own stories: **C-195** (approval-sheet
  redaction, from C-185) and **C-205** (bump `lru`, drop its unsound-advisory ignore, from C-188).
  Also landed earlier the same day: **C-185** (the shared-redactor diff-marker fix).
- ~~**Still open before this epic closes:** (1) **C-191** … (2) the **re-run of the
  `adversarial-review` skill** … (3) the **sandbox-default deferral**.~~ All three are discharged:
  C-191 is `done` (`d25eeab6`), the deferral became **C-217** (`done`, `f616b1ff`), and the re-run is
  **C-267**.
- 2026-07-30 — **C-267 landed the closure artifact**:
  [`reviews/single/2026-07-30-security-assurance-closure.md`](../reviews/single/2026-07-30-security-assurance-closure.md),
  verified against the tree at `0.38.0` (`588144a2`) rather than against these story files. Desk-review
  findings 1–4 and classification trust are **closed with evidence** and each control was separately
  checked for **production reachability** — no child marked `done` was found with an absent or
  structurally unreachable control. Assurance moved 5/10 → 7.5/10 and is no longer this project's
  weakest axis; the 8-vs-5 spread that defined the epic is now 8.5-vs-7.5.
- **Why this epic is `in-progress` and not `done`.** Three things are outstanding, and the epic must
  not close over them silently:
  1. **Envelope-integrity finding 4 is OPEN and was never filed.** `file_stat` still reads the whole
     target a second time and discards it — `crates/flux-tools/src/extra.rs:96-107`, the discard at
     `:107`, the promised "note below" still absent from the emitted result. LOW and *not* a security
     defect (the guarded read is correct, and the author deliberately declined `std::fs::metadata` on
     the raw path). It survived not by decision but because no story was written: C-192/193/194 map to
     envelope findings 1–3 and finding 4 fell off the edge with no "won't do" and no reason recorded.
     That is a process gap, and it is the gap that would drop a HIGH finding next time. **It needs its
     own story.**
  2. **C-266 is a `ready` child** — neither side of C-262's fail-closed sandbox switch is proven in
     CI. An epic with a ready child is not done.
  3. **C-205 stays deliberately unclosed** — see the bullet below.
- **The epic's one deliberately unclosed child: C-205** (`blocked`), and it is a defensible call, not
  an oversight. `lru 0.12.5` is transitive via **`ratatui 0.29.0`** (`cargo tree -i lru --workspace`
  → `lru v0.12.5 └── ratatui v0.29.0`; flux declares no `lru` dependency), so reaching `>= 0.16.3`
  needs a breaking **`ratatui 0.30.x`** upgrade — a TUI-wide change, not a lockfile bump. The
  advisory (RUSTSEC-2026-0002) is ***unsound*-class, not a vulnerability**, and is reachable only
  through `LruCache::iter_mut`, which flux never calls (every `iter_mut` in `crates/` is on a
  `Vec`/slice/`HashMap`). The suppression is honest rather than silent: a stated reason in **both**
  tools (`deny.toml:74`, `security-audit.yml:83`), and `deny.toml:50-51` forbids a bare id. Stated
  plainly: flux ships a crate carrying a known unsound advisory, in exchange for not taking a
  breaking UI-framework major bump for an unreachable defect.
- **C-195 closed as WON'T DO, not as implemented** — recorded here because the board shows `done` and
  a reader could otherwise assume the approval sheet gained redaction. It did not, deliberately:
  redaction is a *boundary* control and the sheet is not a boundary. Argued in the design doc and at
  the seam (`crates/flux-tui/src/toolview.rs:191-201`), and **pinned by a test that fails if it is
  ever quietly reversed** — `diff_does_not_redact_credentials_by_decision` (`toolview.rs:468`).
- **The sandbox default is PARTIALLY closed, and the halves differ.** `on` no longer degrades
  *silently* (C-217, asserted against the real binary in `crates/flux-cli/tests/sandbox_posture.rs`),
  and C-262 (C-255 epic) went further than this epic planned: unattended and serving surfaces now
  start at `Require` and refuse to run unconfined (`crates/flux-cli/src/dispatch.rs:156-161`), with
  sandbox network defaulting **closed**. Still open exactly as the baseline stated it: the
  **interactive** default remains `Off` with network open (`flux-system/src/sandbox.rs:63-64`, pinned
  at `:1288`), and **Windows still has no backend**. So the dangerous half — unattended
  auto-approving execution running unconfined — is closed; the default itself is not.
- ⚠ **Do not read C-255's closure as this epic's.** C-186 traces to the **2026-07-29** desk +
  envelope-integrity reviews; C-255 traces to the **2026-07-30** three-review round (`bcfab0ad`,
  against `cb3bb057`, shipped in `0.38.0`) and has its own outstanding closure bullet. Where C-255
  work closed a 2026-07-29 finding this epic did not — C-259 provenance, C-260/C-261 rate limits,
  C-263 the `syn` I/O scanner, C-264 CodeQL/Miri, C-262 the fail-closed profile — the closure artifact
  attributes it to C-255 rather than letting this epic absorb the credit.

## Notes
- **Why C-187 leads.** It is the only finding exploitable by a third party with no flux bug and no
  operator mistake. The plugin trust model's per-artifact SHA-256 chain terminates in a Minisign
  signature whose key (`MINISIGN_SECRET_KEY`) lives in workflows that run unpinned third-party
  actions. Compromise there invalidates the signing story retroactively.
- **Deferred: the sandbox default.** The review's headline finding (sandbox `Off` by default,
  network open — `flux-system/src/sandbox.rs:39,:50,:64`, pinned by the test at `:1151`) is real but
  is a product decision, not a bug. Flipping it while `on` still degrades silently to unconfined
  (`:463`) would manufacture false assurance — worse than an honest `off`. Correct sequence: make
  `on` report its resolved posture loudly first, then revisit the default with its own design doc
  covering the Windows gap (no backend exists — only Bubblewrap and Seatbelt are implemented).
- **Out of scope by nature:** bus factor, adoption, and "get an external audit". Real risk, but no
  code change addresses them; they are context for the score, not to-dos.
