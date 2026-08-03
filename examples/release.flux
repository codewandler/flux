# examples/release.flux — cut a flux release, as a Flux-Lang program (C-251).
#
# The division of labour this program exists to enforce: the MODEL writes prose, the HOST decides the
# version. crates.io is yank-only, so a model that reads a diff and calls a breaking change "patch"
# has published it permanently across 30 crate names. The signal is already mechanical — this repo
# marks breaking commits with a conventional-commit `!` — so `release_plan` derives the bump with a
# regex and nothing here ever reads a version back out of a model reply.
#
# The program stops at a LOCAL annotated tag. Pushing, the GitHub release and crates.io stay with the
# existing tag-triggered workflows (BUILD-ONCE candidate → promote), so a bug in this program cannot
# publish anything.
#
# Its authority is narrow by construction rather than by prompt: the only process-capable ops it calls
# are `release_verify_versions` and `release_cut`, whose argv is fixed to the two named scripts, and
# the only writing op is `changelog_insert`, which refuses any path outside the three release
# changelogs. It never calls `bash`, `proc.run`, `write`, or `edit`.
#
# Run with: flux flow run examples/release.flux --arg apply=false --yes
# `apply=false` is the safe local/manual default: it derives the version, renders both changelog
# sections and the diff it would apply, and mutates nothing — no file, no commit, no tag. The
# release-branch workflow supplies `apply=true`; its host-owned step promotes the resulting local cut
# only after the exact-SHA candidate and receipt are green.

flow release(apply: Bool) -> String
goal "Cut a flux release: the host derives the version from the commit titles, a scribe drafts both changelogs, the host inserts that prose, and the run stops at a local annotated tag."

  # ── 1. Ground truth from git, and the host's decision ───────────────────────
  # One op, so there is exactly one place a release version can come from: the last `v*` tag, the
  # commit subjects since it, the diffstat, and the bump those subjects imply.
  $plan = release_plan({})
  $count = $plan.commit_count
  $last_tag = $plan.last_tag
  $bump = $plan.bump
  $log = $plan.log
  $diffstat = $plan.diffstat

  # Nothing to release is a success, not a failure — a second run on an already-released SHA must be
  # a no-op rather than an empty release.
  when $count == 0
    return fmt("no commits since {last_tag}; nothing to cut")

  # ── 2. Fail closed on the protocol line, before anything is written ─────────
  # The `codewandler-flux-{spec,secret,policy,evidence,datasource,plugin-protocol,host-kit}` crates
  # version the WIRE on their own 1.x line (C-143). A model must never reason about wire
  # compatibility, so an unbumped protocol crate ERRORS here — naming the crate, and halting before a
  # single changelog byte moves. This is deliberately the first thing after reading git.
  $versions = release_verify_versions({})

  # ── 3. The scribe curates prose. It holds no tools; it returns text. ────────
  # Two audiences, two voices: `CHANGELOG.md` is for engineers (mechanism, files, why),
  # `WHATS-NEW.md` is for users (plain language, no story IDs, no crate names).
  $notes_text = task({ role: "release-scribe", task: fmt("""
Write the release notes for flux — a Rust agent SDK, harness, and coding agent — for the release
after {last_tag}.

Commit subjects since {last_tag}:
{log}

Diffstat:
{diffstat}

Return ONLY a JSON object, no prose and no code fences:
{"changelog": "...", "whats_new": "...", "bump_opinion": "patch|minor", "bump_reason": "..."}

`changelog` is for ENGINEERS: what changed and WHY, naming the files and the mechanism. Group under
`### Added` / `### Changed` / `### Fixed`. A breaking item says so in its first sentence.

`whats_new` is for USERS of flux: plain language, feature-first, what someone can now do or what
behaves differently. NO story IDs, NO crate names, NO internal jargon. Use `### Action needed` for
anything breaking, phrased as the action to take.

`bump_opinion` is ADVISORY ONLY — the host has already derived the version from the commit titles and
your answer cannot change it. If you disagree, say why in `bump_reason`; the run will surface it.
""") })
  # `task` returns text even when the prompt requests JSON. Parse it through a pure, strict host op
  # so prose/fences/schema drift halt here instead of becoming a confusing field-access failure.
  $notes = release_parse_notes({ text: $notes_text })
  $changelog = $notes.changelog
  $whats_new = $notes.whats_new
  $opinion = $notes.bump_opinion
  $reason = $notes.bump_reason

  # The scribe's opinion is a review signal, never the decision. A disagreement has to be loud — an
  # observation for the audit trail and a warning in the returned summary — and it must not move the
  # number, because the number is the irreversible part.
  $warning = fmt("")
  when $opinion != $bump
    observe({ kind: "release.bump_disagreement", data: { host: $bump, model: $opinion, reason: $reason } })
    $warning = fmt("WARNING: the scribe argued for {opinion} ({reason}); the host derived {bump} from the commit titles and that is what was cut.")

  # ── 4. The HOST inserts the prose ───────────────────────────────────────────
  # Deterministic, anchored on `## [Unreleased]`, and idempotent. The model's text is an INPUT to this
  # step, never the file content: if the model wrote the file, one injected instruction would be
  # editing the release notes. `apply: false` returns the diff and writes nothing.
  $cl = changelog_insert({ file: "CHANGELOG.md", section: "Unreleased", body: $changelog, apply: $apply })
  $wn = changelog_insert({ file: "WHATS-NEW.md", section: "Unreleased", body: $whats_new, apply: $apply })
  $cl_diff = $cl.diff
  $wn_diff = $wn.diff

  # ── 5. Cut ──────────────────────────────────────────────────────────────────
  # `scripts/cut-release.sh` owns the version sweep, the re-lock, the `[Unreleased]` roll, the tested
  # website mirror, the commit and the annotated tag — and it is transactional (C-147): its gate runs
  # last, over exactly what is about to be tagged, and any non-zero exit restores every file it
  # touched. So a red gate here leaves no phantom version section and no tag. It is called, not
  # wrapped and not reimplemented.
  $cut = release_cut({ bump: $bump, apply: $apply })
  $version = $cut.version
  $tag = $cut.tag
  $action = $cut.action

  return fmt("""
release {version} ({bump}) since {last_tag} — {action}. Tag {tag} is LOCAL; CI promotes it.
{warning}

── CHANGELOG.md ──
{cl_diff}
── WHATS-NEW.md ──
{wn_diff}
""")
