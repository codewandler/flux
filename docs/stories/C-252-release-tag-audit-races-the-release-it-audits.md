---
id: C-252
title: "The release-tag audit races the release it audits, so every cut reds CI on main for a minute"
pillar: Core
status: done
areas: [ci]
note: "found cutting 0.37.0: ci on cedef3f4 was red purely because the push-triggered audit ran at 22:23 and v0.36.0's Release was published at 22:24:19 — a red that resolves itself and therefore teaches people to ignore red CI"
---

# The release-tag audit races the release it audits, so every cut reds CI on main for a minute

## Goal
`scripts/check-release-tags.sh` (job `release-tags` in `ci.yml`) audits the whole fleet on every push
to `main`: every `vX.Y.Z` tag must have a Release, and `/releases/latest` must be the newest released
version. It is a good check — it exists because `verify-github-release.sh` structurally cannot catch a
tag whose workflow died before the verification step, which is how N-001 survived until an external
tester reported it.

But it **races the release it audits**. A cut pushes `main` and then pushes the tag; the tag workflow
builds and publishes the Release *asynchronously*, while the push to `main` immediately triggers
`ci.yml` — including this audit. So the audit sees a tag that does not have a Release *yet* and fails
correctly-but-uselessly.

Observed while cutting 0.37.0: `ci` on `cedef3f4` was **red**, and the only failing job was
`every version tag has a Release and /releases/latest is newest`. The push-triggered run started at
`2026-07-29T22:23:05Z`; `v0.36.0`'s Release was published at `2026-07-29T22:24:19Z` — 74 seconds
later. Re-running the same script by hand afterwards passes: `94 released version tag(s),
/releases/latest = v0.36.0`.

**Why this is worth fixing rather than tolerating:** a red that resolves itself on every release
teaches everyone to ignore red CI on `main`, which is exactly the habit that let N-001 survive. A
check whose false-positive rate is "once per release" is training people out of trusting it.

## Acceptance
- [x] Cutting a release does not produce a red `ci` on `main` from this job. **Failing-first test**:
      the script's `--self-test` gains a case where the newest tag has no Release *and* its release
      workflow is still in progress — it must not fail today.
- [x] A tag that genuinely has no Release and no in-flight workflow **still fails**, loudly. The fix
      must not be "ignore the newest tag", which would blind the check to exactly the N-001 shape it
      exists to catch.
- [x] The distinction is made on evidence, not on a sleep: either the tag's release workflow is still
      running (queryable), or the tag is younger than a stated grace window with the window and the
      reason printed. Whichever is chosen, the script says which case it took.
- [x] `ALLOWED_WITHOUT_RELEASE` semantics are unchanged (`v0.11.1`, `v0.12.0`, `v0.17.0` stay
      deliberate exemptions, each verified from its run).
- [x] Exit 2 still means "GitHub unreachable ⇒ CI skip" and is not conflated with a real failure.

## Progress
- 2026-07-30 — found while cutting 0.37.0, and it will recur on this cut too: the same job will go red
  on `a0ad8219` until `v0.37.0`'s Release publishes. Not a blocker for the cut; recorded so the next
  person does not chase it as a regression.
- 2026-07-30 — **implemented shape 1: ask GitHub whether that tag's Release workflow run is in
  flight.** `scripts/check-release-tags.sh` gains two functions — `release_runs_for_tag` (the one
  network call: `actions/runs?branch=<tag>`, filtered to `.github/workflows/release.yml`, emitting
  `status<TAB>conclusion` per run) and `classify_release_gap` (pure, so the self-test stays hermetic).
  Every gap `missing_releases` reports is classified before anything is failed:
  - a run that has not reached `completed` → `pending`: a yellow `NOTE` naming the run state, **not**
    a failure, and counted on the PASS line as `N still publishing`;
  - a run that reached `completed` without a Release → `missing`, naming the conclusion;
  - **no run at all** → also `missing`. "No evidence" must not read as "still building", or a tag
    pushed with workflows disabled would defer forever.

  So the deferral is narrow by construction and the N-001 shape stays caught **on the newest tag**.
  Nothing anywhere in the script knows which tag is newest — that was the trap, and avoiding it is
  what makes this net-positive rather than net-negative.

  **Rejected — shape 2, a grace window on tag age.** It is a guess dressed as evidence, and it fails
  in both directions: a `cargo-dist` build of five targets can exceed any window a human would pick
  (so the red comes back), and inside the window a genuinely dead cut is forgiven with no way to tell
  the two apart. It also cannot distinguish "queued behind a busy runner" from "the run failed
  ninety seconds ago", which is precisely the distinction the story asks the output to print.

  **Rejected — shape 3, move the audit to a schedule.** Cheapest, and it does remove the red, but it
  trades the per-push guarantee for a window in which drift is live and unreported. The guarantee is
  most of the job's value: N-001 survived because *nothing* was watching between releases.

  **Residual gap, stated rather than papered over:** GitHub creates the workflow-run row a beat after
  it creates the tag ref, so there is a seconds-wide window in which the tag is visible to
  `repos/:r/tags` and its run is not yet listed — classified `missing`, i.e. a red. That is ~2s of
  exposure against the ~90s this story is about, and closing it would mean reintroducing exactly the
  tag-age timing assumption shape 2 was rejected for. It errs toward failing, which is the correct
  direction for this check.

  Also: `ci.yml`'s `release-tags` job now declares `permissions: {contents: read, actions: read}` —
  the run query needs `actions: read`, and a job-level block replaces the repo default outright, so
  `contents: read` for checkout is spelled out too.

  Verified against **live** GitHub, not only the self-test:
  - allowlist temporarily emptied → `FAIL` for `v0.17.0`/`v0.12.0`/`v0.11.1`, each line reading
    `its newest Release workflow run finished (failure) without publishing a Release`. Real run data,
    real conclusions. This is the proof that a genuine gap still fails loudly.
  - `release_runs_for_tag` pointed at a Release run that was genuinely `in_progress` at the time
    (0.37.0's candidate build) → `pending  its Release workflow run is in_progress`.
  - the unmodified allowlist path is untouched: the three exempt tags are filtered out by
    `missing_releases` before the classifier is ever called, so they cost no API call and their
    semantics cannot have changed.
  - runs-list read failure with a real gap present → `skip: ...`, exit 2.

## Notes
- Fix shapes, in preference order:
  1. **Ask GitHub whether the release workflow for that tag is in flight** — most precise, no timing
     assumption; the run is queryable by tag ref.
  2. **A grace window on tag age** (e.g. a tag younger than ~15 minutes may lack a Release), printed
     explicitly. Simpler, but it is a timing assumption and a slow multi-platform build can exceed it —
     `cargo-dist` builds five targets.
  3. Move the audit off the push trigger to a schedule. Cheapest, but it stops being a per-push
     guarantee, which is most of its value.
- The deeper asymmetry, worth stating in whatever fix lands: **the tag workflow and the push workflow
  are independent**, so anything that reasons about "the state after a release" from a push-triggered
  job is racing by construction. See [[the release flow is BUILD-ONCE: candidate → promote]] — the
  candidate/promote split means the artifacts exist before the tag, but the *Release object* still
  appears only after the tag is pushed.
- Related and deliberately separate: **C-251** would automate cutting entirely. If this race is not
  fixed first, that automation will produce a red `ci` on `main` on every automated release, which is
  worse than today because nobody will be watching.
