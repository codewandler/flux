# examples/release-cut.flux — deterministic, credential-free automatic release cut (C-251).
#
# The release branch must never depend on a model provider, account balance, or generated prose.
# Release notes land with the changes they describe under the two checked `[Unreleased]` sections.
# This host-only program derives the irreversible version from repository evidence, validates the
# independently versioned wire crates, and delegates the transactional roll/commit/tag to the one
# existing release script. It contains no `task`, model, network, general process, or general write
# operation.
#
# The program stops at a LOCAL annotated tag. The separate host-owned controller imports the exact
# cut bundle, merges it to canonical main through CI, gates the merged SHA as a candidate, and only
# then pushes the tag that starts publication.

flow automatic_release(apply: Bool) -> String
goal "Cut a Flux release from already-reviewed repository state without any model or provider credential."

  # One host op reads the fully framed commit records, the customer-facing Action-needed signal,
  # and Cargo.toml. It alone derives patch/minor/major and the next version.
  $plan = release_plan({})
  $count = $plan.commit_count
  $last_tag = $plan.last_tag
  $bump = $plan.bump
  $next = $plan.next_version

  # Re-running an already released tagged checkout is a no-op.
  when $count == 0
    return fmt("no commits since {last_tag}; nothing to cut")

  # Protocol-line crate versions remain a human-owned decision and fail before any file changes.
  $versions = release_verify_versions({})

  # `cut-release.sh` owns the version sweep, lockfile, changelog roll, public-doc regeneration,
  # commit, and annotated local tag. It is transactional before commit. A manual rehearsal runs the
  # full gate here; only the automatic release-branch push delegates that same gate to the exact-SHA
  # candidate whose immutable receipt is required before promotion.
  $cut = release_cut({ bump: $bump, apply: $apply })
  $version = $cut.version
  $tag = $cut.tag
  $action = $cut.action

  return fmt("release {version} ({bump}) since {last_tag} — {action}. Expected {next}; tag {tag} is local until the candidate passes.")
