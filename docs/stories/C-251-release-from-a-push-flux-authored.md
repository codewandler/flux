---
id: C-251
title: "Cutting a release should be a push — a Flux-Lang program curates the changelogs, the host decides the version"
pillar: Core
status: in-progress
priority: 10
areas: [flux-cli, flux-tools, ci]
note: "flux automating its own release is the most honest dogfood there is; the load-bearing decision is that the MODEL writes prose and the HOST does version math, because a wrong version on crates.io is irreversible"
---

# Cutting a release should be a push — a Flux-Lang program curates the changelogs, the host decides the version

## Goal
Today a release is a human sequence: pre-validate the gate, run `scripts/smoke-live.sh` with a real
key, hand-write `CHANGELOG.md` and `WHATS-NEW.md` entries, decide the version, run
`scripts/cut-release.sh`, push main, run the candidate workflow, verify its SHA, push the tag. It is
well documented and still error-prone — the failure archive includes phantom versions from a re-run
after a rolled changelog, a global `sed` that bumped an unrelated external crate, and a backfilled
tag that hijacked `/releases/latest`.

Make **merging main into `release` the whole release action.** CI then: runs the smoke test against a
cheap model over OpenRouter, hands the diff to a flux agent that drafts and polishes the two
changelogs in the house voice, has the **host** derive the next version mechanically, cuts, tags, and
lets the existing tag-triggered workflows publish. Expressed as a `.flux` program, because the loop is
exactly what Flux-Lang is for — and because flux automating its own release is the most honest dogfood
available.

## The load-bearing decision: the model writes prose, the host decides the version

The request was for the agent to "figure out the next version". **This story deliberately does not do
that**, and the reason is the same principle the fleet-loop epic is built on — *the model reasons, the
host enforces*:

- **A wrong version is irreversible.** crates.io is yank-only. A model that reads a diff and says
  "patch" when a trait gained a method without a default body has published a breaking change under a
  compatible version number, to 30 crate names, permanently.
- **The signal is already mechanical.** flux's rule is *breaking → MINOR (while 0.y), additive and
  fixes → patch*, and the repo already marks breaking commits with a conventional-commit `!`
  (`refactor(events,sdk,cli)!:`, `feat(capabilities,tools)!:`). Deriving the bump from commit titles is
  a regex, not a judgement.
- **The protocol line has a second, independent rule** that `scripts/check-crate-versions.sh` already
  enforces: the `codewandler-flux-{spec,secret,policy,evidence,datasource,plugin-protocol,host-kit}`
  crates version *the wire*, on their own 1.x line. When that script fails, the flow must **stop**,
  not guess — a model cannot be trusted to reason about wire compatibility, and this is precisely the
  check that catches it.

So the model's job is the part it is genuinely good at and the host is bad at: reading a diff and
writing prose a human would want to read, in two different voices — engineer-facing `CHANGELOG.md`
(with `path:line` and *why*) and customer-facing `WHATS-NEW.md` (plain language, no story IDs, no
crate names). The model may also **explain** the version and **disagree in writing**, which fails the
run loudly rather than silently changing the number.

## Acceptance
- [ ] Merging `main` → `release` produces a tag and a GitHub release with no human step, or fails
      loudly with the reason. **Failing-first test**: the program's offline journey — a fixture repo
      with a known commit log, a stub model, and no network — produces the expected version, the
      expected two changelog sections, and a tag; and produces **no tag** when the gate is red.
- [x] The version is derived by the host from commit titles (`!` / `BREAKING`), never from model
      output. A test pins: a log containing `feat(x)!:` yields `minor`; a log of only `fix:`/`docs:`
      yields `patch`; and a model reply asking for a different bump does **not** change it.
- [x] `scripts/check-crate-versions.sh` failing **halts the flow before any tag exists**, with the
      protocol-line crate named. Pinned by a test.
- [x] The model's prose is inserted under `[Unreleased]` deterministically by the host, and
      `website/docs/whats-new.md` is regenerated in the **same commit** — the mirror is a tested input
      (`website_customer_changelog_is_in_sync`), so a bare `WHATS-NEW.md` edit is a red gate.
- [x] The program runs under a **narrow, runtime-enforced authority ceiling by construction**: its
      op set exposes no general write or process tool; `changelog_insert` canonicalizes and permits
      exactly `CHANGELOG.md`, `WHATS-NEW.md`, and `website/docs/whats-new.md`; the two process-capable
      release ops use fixed argv for the named scripts; and the scribe has `tools: []`. Attempts to
      write elsewhere are refused by the operation implementation, not by prompt or by the decorative
      `.flux/policies/release.toml`. Pinned by `release_authority.rs` against the shipped AST, path
      boundary, permission subjects, and role.
- [x] The smoke test is wired in CI against the configured cheap direct Anthropic model
      (`FLUX_SMOKE_MODEL`), and its failure blocks the cut. An explicit `openrouter/*` override
      selects its own credential. Optional legs whose credential is absent SKIP; an automatic
      release with no selected-provider credential fails loudly rather than finishing green after
      doing nothing. The source wiring is complete; live hosted proof remains pending and is tracked
      below.
- [x] The flow is idempotent and re-runnable: a second run on an already-released SHA is a no-op, and
      a failed run leaves **no** partially-rolled changelog (the C-147 transactionality property that
      `cut-release.sh` already has must not be lost by wrapping it).
- [x] Standard gate green in both workspaces.

## First draft of the `.flux` part

Text syntax, per `crates/flux-lang/docs/syntax.md`. **This is a draft, not a specification** — the op
names marked `⚠` need checking against the live catalog before implementation, and the file-writing
seam is the least certain part.

```flux
flow release -> string
  goal "Cut a flux release from a merge into `release`: gate, curate the changelogs, let the host
        decide the version, tag. The model never decides the version and never tags."

  # ── 1. Ground truth from git, not from a model ────────────────────────────────
  $last_tag = do proc.run { argv: ["git", "describe", "--tags", "--abbrev=0"] }
  $range    = fmt("{last_tag}..HEAD")
  $log      = do git_log { range: $range, format: "%s" }
  $diff     = do git_diff { range: $range, stat: true }

  # Nothing to release is a success, not a failure — this flow fires on every merge.
  $n = do proc.run { argv: ["bash", "-lc", fmt("git rev-list --count {range}")] }
  when $n == "0"
    return fmt("no commits since {last_tag}; nothing to cut")

  # ── 2. The HOST derives the bump. flux rule: breaking -> MINOR while 0.y. ─────
  # A conventional-commit `!` or a BREAKING line is the repo's existing, mechanical
  # breaking signal. This is a regex, deliberately not a judgement.
  $breaking = regex_extract { s: $log, pattern: "(?m)^[a-z]+(\\([^)]*\\))?!:|BREAKING", all: true }
  $bump = fmt("patch")
  when $breaking
    $bump = fmt("minor")

  # ── 3. Fail closed on the protocol line BEFORE anything is written ───────────
  # These crates version the WIRE on their own 1.x line. A model must never reason
  # about wire compatibility, so an unbumped protocol crate halts the run.
  $versions = do proc.run { argv: ["scripts/check-crate-versions.sh"] }
  assert $versions.ok
    "a protocol-line crate changed without a version bump — a human must decide this"

  # ── 4. Gate + smoke, before any release artifact exists ─────────────────────
  parallel
    $gate  = do gate_check { workspace: true }
    $smoke = do proc.run { argv: ["scripts/smoke-live.sh"], timeout_s: 1800 }
  assert $gate.ok  "gate red — refusing to cut"
  assert $smoke.ok "live smoke failed — refusing to cut"

  # ── 5. The model curates prose. It cannot write files; it returns text. ─────
  $notes = task {
    role: "release-scribe",
    task: fmt("""
      You are writing release notes for flux, a Rust agent SDK and coding agent.

      Commit subjects since {last_tag}:
      {log}

      Diffstat:
      {diff}

      Return ONLY JSON: {"changelog": "...", "whats_new": "...", "bump_opinion": "patch|minor",
      "bump_reason": "..."}.

      `changelog` is for ENGINEERS: what changed and WHY, naming files and the mechanism. Group
      under `### Added` / `### Changed` / `### Fixed`. Breaking items say so in the first sentence.
      `whats_new` is for USERS of flux: plain language, feature-first, say what someone can now do or
      what behaves differently. NO story IDs, NO crate names, NO internal jargon. Use
      `### Action needed` for anything breaking, phrased as the action to take.
      `bump_opinion` is advisory only — the host already decided. If you disagree, say why in
      `bump_reason`; the run will surface it.
    """)
  }

  # The model's opinion is a review signal, never the decision.
  $opinion = $notes.bump_opinion
  when $opinion != $bump
    observe { kind: "release.bump_disagreement",
              data: { host: $bump, model: $opinion, reason: $notes.bump_reason } }

  # ── 6. Host inserts the prose and regenerates the tested mirror ─────────────
  # ⚠ seam to confirm: the guarded write op, and whether insertion should be a
  # small script (deterministic anchor on `## [Unreleased]`) rather than a tool.
  do changelog_insert { file: "CHANGELOG.md",  section: "Unreleased", body: $notes.changelog }
  do changelog_insert { file: "WHATS-NEW.md",  section: "Unreleased", body: $notes.whats_new }
  do proc.run { argv: ["bash", "-lc",
       "UPDATE=1 cargo test -p codewandler-flux-lang --test website_in_sync"] }

  # ── 7. Cut. The existing script owns version math, re-locking and the tag. ──
  $cut = do proc.run { argv: ["scripts/cut-release.sh", $bump, "--no-gate"] }
  assert $cut.ok "cut-release failed — tree is restored (C-147), no phantom section"

  $version = regex_extract { s: $cut, pattern: "cut ([0-9]+\\.[0-9]+\\.[0-9]+)", group: 1 }
  return fmt("cut {version} ({bump}); tag is local — CI promotes it")
```

**What the program deliberately does not do:** push, create the GitHub release, or publish to
crates.io. Those stay in the host-owned CI half and the existing tag-triggered workflows. The
program's job ends at a local annotated tag. The workflow then stages the cut commit on the exact
`release-candidates/vX.Y.Z` ref, dispatches and watches the candidate build, verifies its
version/SHA/run receipt, and only then advances `main` and pushes the tag. It watches both tag
workflows and runs the public Release verifier before reporting success. Keeping that irreversible
half outside the model-authored program means a bug in the program cannot publish directly.

## Progress
- 2026-08-03 — Hosted preview `30833603707` passed the complete live smoke under bubblewrap, then
  failed closed before writing or promotion because `task()` returned JSON as text and the flow read
  it as an object. `release_parse_notes` now makes that boundary explicit and strict. Unit tests
  reject prose, missing/extra fields, empty engineering notes and invalid bump opinions; the
  shipped-flow journey proves malformed scribe text leaves both changelogs and every ref untouched.
  Preview `30834939427` then passed the complete live smoke and proved Haiku wraps an otherwise exact
  object in a canonical `json` fence despite the no-fence instruction. The host now normalizes only
  that exact transport wrapper before applying the same schema. Internal-only releases retain their
  documented ability to omit customer-facing prose.
- 2026-08-03 — **the unattended path is implemented in source and the story is now `in-progress`,
  but Acceptance item 1 remains deliberately unchecked until a hosted run dogfoods it.**
  `.github/workflows/release-flow.yml` now runs automatically only for pushes to `release`, forces
  apply mode for that event, runs the cheap-model smoke before the flow, scopes `RELEASE_TOKEN` to
  the promotion step, and fails rather than silently skipping an automatic release when the selected
  provider credential is absent. Manual dispatch remains the preview/rehearsal surface.
  `scripts/promote-release-flow.sh` owns the irreversible host sequence: validate the local annotated
  tag; stage its exact SHA at `refs/heads/release-candidates/vX.Y.Z`; dispatch and watch
  `release.yml`; verify the immutable candidate receipt; advance `main`; push the tag; watch the
  binary and crates.io workflows; verify the public GitHub Release; then delete only the exact
  candidate ref. Failures before main retain the candidate ref and leave main/tag untouched; failures
  after the tag retain recovery evidence.
  `scripts/test-promote-release-flow.sh` exercises the happy path, ref ordering, receipt mismatch or
  absence, candidate/build/publication failures, stale-ref refusal, merge ancestry, idempotent no-op,
  and token non-disclosure with hermetic `git`/`gh` fixtures. `release_authority.rs` composes the
  workflow and helper source to pin the same ordering, the trigger-capable credential boundary, and
  `release.yml`'s exact versioned candidate-ref admission rule. **Operational activation is still
  pending:** these paths have not yet completed a real `main` → `release` hosted cut with configured
  secrets and a publicly verified Release. That dogfood run is the evidence required to tick item 1
  and move this story to done.
- 2026-08-03 — Two fail-closed hosted previews improved the activation boundary before any ref could
  move. Run `30831706707` proved the original OpenRouter default depended on account credits; run
  `30832802801` proved the stock hosted runner has no sandbox backend, so the agentic and served live
  smoke legs correctly refuse to start. The workflow now defaults to direct Anthropic Haiku and
  provisions plus self-tests bubblewrap before running Flux. Structural tests pin both requirements.
- 2026-08-03 — Run `30833459849` installed bubblewrap but proved Ubuntu 24.04's hosted AppArmor
  default denies its UID map. The workflow now enables unprivileged user namespaces only on the
  dedicated ephemeral runner and immediately proves a minimal bwrap namespace before compilation.
  It does not disable Flux's sandbox.
- 2026-07-30 — **the foundation is merged and this story stays `ready` for the rest.** Recovered as an
  orphan after a coordinating session crash killed its implementor mid-task; branch preserved verbatim,
  reviewed independently, four blocking findings discharged, then integrated.
  **Ticked (2, 3, 4, 7, 8) — each with a named test:** the host derives the bump and the model cannot
  move it (`a_breaking_title_derives_a_minor_bump`,
  `a_scribe_asking_for_a_different_bump_does_not_change_the_number`); an unbumped protocol-line crate
  halts before anything is written (`an_unbumped_protocol_line_crate_halts_before_anything_is_written`);
  the host inserts the prose (`an_applied_run_inserts_the_prose_and_produces_exactly_one_new_tag`); and
  the flow is re-runnable and transactional (`a_second_run_on_an_already_released_sha_is_a_no_op`,
  `a_red_gate_in_the_cut_leaves_no_tag_and_no_phantom_version_section`). Gate green on the integration
  branch.
  **Item 1 — NOT met, deliberately.** `.github/workflows/release-flow.yml` is `workflow_dispatch` only,
  `permissions: contents: read`, `apply` defaulting to false. No `push:` trigger, no tag push, no
  GitHub release. This is the right first step rather than a shortfall: an unattended auto-cut is the
  hazard C-252 was just fixed to avoid, and this posture cannot push a tag at all, so it can never
  produce a remote Release-less tag. Remaining work is the trigger and the promotion path.
  **Item 5 — NOT met, and the wording needs correcting when it is picked up.** `.flux/policies/release.toml`
  is **decorative**: no crate, script, workflow or program loads it — the only references anywhere are the
  three lines in `release_authority.rs` that read and parse it, so
  `the_checked_in_release_policy_grants_exactly_the_three_changelogs_and_two_scripts` verifies a *document
  against constants*, not runtime enforcement. There is no path-scoped policy floor at runtime, because
  `flux-cli` composes `[[policy.grants]]` additively on top of `default_local_grants()`, which already
  grants `workspace.write` on `path: "*"`. What *does* refuse structurally is the **op set** — fixed-argv
  process ops, `changelog_insert`'s canonicalized allow-list, and `tools: []` on the role — and that is
  genuinely stronger than a policy rule, since no policy composition can widen it. So "refused
  structurally by policy" should be reworded to credit the op set, or a real path-scoped floor should be
  installed. Do not read the merged state as having a policy floor.
  **Item 6 — NOT met/unverified.** No CI smoke leg against `FLUX_SMOKE_MODEL` exists yet, and the
  workflow cannot be verified end to end here (it needs a credential and a dispatch). One `apply: false`
  dispatch would settle it now that the role file is tracked.
  **The blocking find worth remembering:** `.flux/agents/release-scribe.md` and
  `.flux/policies/release.toml` were gitignored and untracked, so two tests passed only because untracked
  files sat beside them in the implementor's worktree, and `task({role: "release-scribe"})` would have
  failed closed with `unknown role` in every other checkout — the feature was inert and the gate would
  have gone red on `main`. Fixed with narrow `.gitignore` negations following the L-14 precedent, and
  proven from the commit via `git archive | tar -x` into a clean directory rather than from the working
  tree. `.flux/plans/`, `.flux/state.json` and scratch roles/policies remain ignored.
  Also noted, not fixed: `PERMITTED_OPS` lists `fmt`/`jq`/`expr`, which never appear in the collected op
  set, and neither guard test asserts the set is non-empty — so a future AST-serialization change could
  defang both silently.
- **Shipped (2026-07-30).** The Flux-Lang program (`examples/release.flux`) plus its host half
  (`crates/flux-eval/src/release.rs`): `derive_bump` reads the bump from commit titles by regex and
  nothing reads a version back out of a model reply, so the version decision never leaves the host.
  Authority is narrow **by construction** rather than by policy rule — the two process-capable ops
  carry fixed argv for `scripts/check-crate-versions.sh` and `scripts/cut-release.sh`, and
  `changelog_insert` resolves its target through `flux-system`'s canonicalizing boundary and refuses
  anything outside the three release changelogs. The scribe role declares `tools: []`, so the model
  that drafts the prose holds no write op to attempt.
  `crates/flux-eval/tests/release_authority.rs` pins each half against the shipped artifacts by
  walking the serialized AST, not against a prompt. The workflow is
  `.github/workflows/release-flow.yml`: `workflow_dispatch` only, `permissions: contents: read`, and
  `apply` defaulting to false.
- **Not shipped, so Acceptance item 1 is NOT met.** There is no `push: release` trigger, no tag push,
  and no GitHub release creation. "Merging `main` → `release` produces a tag and a GitHub release with
  no human step" does not describe what landed: a release is still cut by a human running
  `scripts/cut-release.sh`, exactly as `crates/flux-sdk/PUBLISHING.md` documents. The dispatch-only
  workflow is a reviewable preview (`apply: false`) or an in-runner rehearsal (`apply: true`); neither
  mode releases anything.
- **The dispatch gate is the right first step, not a shortfall.** An unattended auto-cut is precisely
  the hazard C-252 was just fixed to avoid, and this posture cannot reach it: with
  `permissions: contents: read` there is no write token in the job, so even `apply: true` cannot move
  a ref on the remote — the commit and annotated tag it creates live and die inside the ephemeral
  runner. A workflow that cannot push a tag can never leave a remote Release-less tag behind, and
  publication (`release.yml`, `crates-io.yml`) is tag-push-triggered, so it cannot publish either.
  Flipping the trigger is a separate, deliberate change, to be made only once this workflow has run
  green by hand a few times.
- **Deliberately left to a follow-up:** the `push: release` trigger, the tag push and the release
  creation — i.e. the unattended half of Acceptance item 1.
- **On `.flux/policies/release.toml`:** it is a checked-in **document**, not an installed policy. No
  code path loads it; `crates/flux-eval/tests/release_authority.rs` reads it only to verify its three
  writable paths and two exec subjects cannot drift from `WRITABLE_CHANGELOGS`/`RELEASE_SCRIPTS`. It
  cannot be a floor today because `flux-cli` composes `[[policy.grants]]` *additively* on top of
  `flux_policy::default_local_grants()`, which already grants `workspace.write` on `path: "*"`
  (`crates/flux-cli/src/execution.rs:1419`). The enforcement the release flow relies on today is the
  op set, which no policy composition can widen.

## Notes
- **Trigger:** a push to `release`, not to `main`. Merging main → release is the deliberate act; an
  ordinary main push must not cut. This was the user's own framing and it is the safer one.
- **Model + key:** direct `anthropic/claude-haiku-4-5` with `ANTHROPIC_API_KEY` is the default. A
  manual `openrouter/*` override selects `OPENROUTER_API_KEY`. The first hosted rehearsal proved the
  distinction matters: OpenRouter returned 402 with no account credits, while its zero-cost models
  were unavailable under the account's privacy policy. The direct provider avoids coupling release
  availability to either condition without weakening that policy.
- **Why this is a good flux story rather than a shell script:** every hard part is something flux
  already claims to do — a narrow runtime op set whose canonical path boundary means a model-driven
  run structurally cannot touch source, fixed-argv process seams through `flux_system`, and an
  auditable action batch. If flux cannot safely automate its own release, the claim that it can
  safely automate someone else's work is weaker.
- **Ops this needs that now exist:** C-238 landed `git_branch`/`git_merge`/`git_revert`, so the
  merge-and-revert half is expressible; `gate_check`, `git_snapshot` and `git_tag` already exist in
  the eval pack. The gap is the changelog-insertion seam (⚠ above) — a deterministic anchored insert
  is probably a small op or a script, and should **not** be the model writing the file, because then
  the model's output becomes the file content rather than its input.
- **No approval prompt exists in CI.** `--yes` makes the run non-interactive, but it does not define
  the ceiling: the model-facing program has only the fixed release op set above, while the separately
  authenticated promotion helper is host-owned and receives `RELEASE_TOKEN` only for its guarded ref
  operations. An unattended agent in a release pipeline is exactly the shape that must fail closed.
- **Failure archive worth reading first** (all real, all in this repo's history): a gate flake *after*
  the changelog roll minted a phantom version section; `cut-release.sh`'s global `sed` bumped an
  external crate that happened to share flux's version string; a backfilled tag hijacked
  `/releases/latest` because that endpoint ranks by `published_at`. An automated cut must not
  reintroduce any of them — `scripts/check-release-tags.sh` already audits the last one on every push.
- **Sequencing:** this depends on nothing, but it is worth landing *after* the fleet-loop epic's F5
  (`fleet.integrate`), because gate-then-act-or-revert is the same shape and F5 makes it a host
  guarantee rather than a program convention.
