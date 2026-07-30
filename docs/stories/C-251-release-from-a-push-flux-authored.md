---
id: C-251
title: "Cutting a release should be a push — a Flux-Lang program curates the changelogs, the host decides the version"
pillar: Core
status: ready
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
- [ ] The version is derived by the host from commit titles (`!` / `BREAKING`), never from model
      output. A test pins: a log containing `feat(x)!:` yields `minor`; a log of only `fix:`/`docs:`
      yields `patch`; and a model reply asking for a different bump does **not** change it.
- [ ] `scripts/check-crate-versions.sh` failing **halts the flow before any tag exists**, with the
      protocol-line crate named. Pinned by a test.
- [ ] The model's prose is inserted under `[Unreleased]` deterministically by the host, and
      `website/docs/whats-new.md` is regenerated in the **same commit** — the mirror is a tested input
      (`website_customer_changelog_is_in_sync`), so a bare `WHATS-NEW.md` edit is a red gate.
- [ ] The program runs under a **narrow, explicit authorization**: write authority path-scoped to
      exactly `CHANGELOG.md`, `WHATS-NEW.md`, `website/docs/whats-new.md`, and process authority to
      the named scripts. Any attempt to write elsewhere is refused **structurally** by policy, not by
      prompt. Pinned by a test that has the model try.
- [ ] The smoke test runs in CI against a cheap OpenRouter model (`FLUX_SMOKE_MODEL`), and its
      failure blocks the cut. Legs whose credential is absent SKIP rather than fail.
- [ ] The flow is idempotent and re-runnable: a second run on an already-released SHA is a no-op, and
      a failed run leaves **no** partially-rolled changelog (the C-147 transactionality property that
      `cut-release.sh` already has must not be lost by wrapping it).
- [ ] Standard gate green in both workspaces.

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
crates.io. Those stay with the existing tag-triggered workflows, which already implement the
BUILD-ONCE candidate→promote flow. The program's job ends at a local annotated tag; the workflow step
after it does `git push origin main`, runs the candidate build, verifies
`candidate headSha == tag SHA`, and only then pushes the tag. Keeping the irreversible half in CI
means a bug in the program cannot publish.

## Notes
- **Trigger:** a push to `release`, not to `main`. Merging main → release is the deliberate act; an
  ordinary main push must not cut. This was the user's own framing and it is the safer one.
- **Model + key:** `OPENROUTER_API_KEY` as a repo secret, with a cheap or free model. Prose curation
  and a smoke turn are both small; see the OpenRouter model spec already used for eval and loop work.
  The smoke test takes `FLUX_SMOKE_MODEL`, so no code change is needed to point it at OpenRouter.
- **Why this is a good flux story rather than a shell script:** every hard part is something flux
  already claims to do — a narrow path-scoped write authority so a model-driven run *structurally*
  cannot touch source (`crates/flux-policy`), a guarded process seam so every command is argv-only
  (`flux_system`), and an auditable action batch. If flux cannot safely automate its own release, the
  claim that it can safely automate someone else's work is weaker.
- **Ops this needs that now exist:** C-238 landed `git_branch`/`git_merge`/`git_revert`, so the
  merge-and-revert half is expressible; `gate_check`, `git_snapshot` and `git_tag` already exist in
  the eval pack. The gap is the changelog-insertion seam (⚠ above) — a deterministic anchored insert
  is probably a small op or a script, and should **not** be the model writing the file, because then
  the model's output becomes the file content rather than its input.
- **No approval prompt exists in CI.** The run needs an explicit non-interactive authorization with
  the narrow policy above. That is the honest hard part of this story and where its design review
  should concentrate: an unattended agent with write authority in a release pipeline is exactly the
  shape that must fail closed.
- **Failure archive worth reading first** (all real, all in this repo's history): a gate flake *after*
  the changelog roll minted a phantom version section; `cut-release.sh`'s global `sed` bumped an
  external crate that happened to share flux's version string; a backfilled tag hijacked
  `/releases/latest` because that endpoint ranks by `published_at`. An automated cut must not
  reintroduce any of them — `scripts/check-release-tags.sh` already audits the last one on every push.
- **Sequencing:** this depends on nothing, but it is worth landing *after* the fleet-loop epic's F5
  (`fleet.integrate`), because gate-then-act-or-revert is the same shape and F5 makes it a host
  guarantee rather than a program convention.
