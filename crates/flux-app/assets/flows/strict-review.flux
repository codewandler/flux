# strict_review.flux — the strict code-review protocol as a real, checked-in Flux-Lang flow
# (docs/designs/strict-review-flows.md; story docs/stories/L-10-strict-review-example-flow.md
# [Phase 1] + docs/stories/L-12-strict-review-typed-artifacts.md [Phase 3]).
#
# Gathers context read-only (git_status/git_diff/read_many), packs it into a budgeted `ctx` for the
# audit trail, fans out to a FIXED set of three restricted reviewer roles (security / correctness /
# maintainability — no filesystem/shell tools), then aggregates their raw JSON findings with a
# single deterministic native op: `review.aggregate` normalizes each entry (quarantining malformed
# ones as `gaps` — never silently dropped, never surfaced as findings), dedupes by a computed
# fingerprint (category+file+line+normalized-title; counting reviewer `agreement`), and ranks by
# severity -> confidence -> agreement with a fingerprint tiebreak for byte-identical ordering across
# runs. Returns a typed `ReviewReport`.

flow strict_review(files: List<String>)

  # ---- read-only context gather ----
  $status = git_status()
  $diff = git_diff()
  $sources = read_many({ paths: $files })

  # Budgeted context pack — an audited manifest of what the reviewers were given (Phase 1: the pack
  # itself is a bookkeeping/audit record; the raw content is interpolated into each task below).
  ctx $review_ctx
    purpose "strict code review — read-only context for bounded reviewer fan-out"
    budget 60000
    include $status, $diff, $sources

  # ---- bounded fan-out: exactly these three reviewer roles, never model-chosen ----
  # (each `task` call's args must fit on one line — the parser is line-oriented — so the reviewer
  # prompt is built first via `fmt`, then passed in by symbol.)
  $security_prompt = fmt("Review this change for SECURITY issues.\n\nFiles under review: {files}\n\nGit status:\n{status}\n\nGit diff:\n{diff}\n\nFile contents:\n{sources}\n\nReturn ONLY the JSON array of findings.")
  $correctness_prompt = fmt("Review this change for CORRECTNESS issues.\n\nFiles under review: {files}\n\nGit status:\n{status}\n\nGit diff:\n{diff}\n\nFile contents:\n{sources}\n\nReturn ONLY the JSON array of findings.")
  $maintainability_prompt = fmt("Review this change for MAINTAINABILITY issues.\n\nFiles under review: {files}\n\nGit status:\n{status}\n\nGit diff:\n{diff}\n\nFile contents:\n{sources}\n\nReturn ONLY the JSON array of findings.")

  parallel
    branch $security
      $security = task({ role: "review-security", task: $security_prompt })
    branch $correctness
      $correctness = task({ role: "review-correctness", task: $correctness_prompt })
    branch $maintainability
      $maintainability = task({ role: "review-maintainability", task: $maintainability_prompt })

  # ---- deterministic aggregation ----
  # Each reviewer's `task` result is raw model text: a clean JSON array when the reviewer obeyed its
  # prompt, fenced/prose-wrapped when it didn't. The three raw outputs go straight to
  # `review.aggregate`, whose normalizer flattens arrays and leniently recovers JSON from string
  # blobs (fences/prose stripped), quarantining anything unrecoverable as `gaps` — a sloppy reviewer
  # degrades the report, it never aborts the flow after the sub-agent spend. (An earlier revision
  # merged the three outputs with `merge` first, which hard-failed on any non-array reviewer blob.)
  #
  # normalize -> dedupe by fingerprint (counting reviewer agreement) -> rank by severity, then
  # confidence, then agreement (fingerprint tiebreak). The model never ranks — `review.aggregate`
  # is the runtime, not the LLM.
  return review.aggregate({ findings: [$security, $correctness, $maintainability], files: $files, reviewers: [ "security", "correctness", "maintainability" ] })
