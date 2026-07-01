# strict_review.flux — the strict code-review protocol as a real, checked-in Flux-Lang flow
# (docs/designs/strict-review-flows.md, Phase 1; story docs/stories/L-10-strict-review-example-flow.md).
#
# Gathers context read-only (git_status/git_diff/read_many), packs it into a budgeted `ctx` for the
# audit trail, fans out to a FIXED set of three restricted reviewer roles (security / correctness /
# maintainability — no filesystem/shell tools, see .flux/agents/review-*.md), then aggregates their
# JSON findings deterministically: merge -> filter (drop malformed entries) -> dedupe (by
# fingerprint) -> sort (by rank desc).
#
# Run with: `flux run examples/strict_review.flux --input '{"files": ["crates/flux-lang/src/ast.rs"]}'`

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
  # Each reviewer's `task` result is stored as a JSON-string. Wrapping the three symbols in a `lists`
  # list-template makes the runtime re-parse each string leaf into a real JSON array (the same
  # mechanism a `jq`/`parse` step relies on), which is what lets `merge` (an array-of-arrays op) see
  # real arrays instead of opaque strings.
  $all_findings = merge({ lists: [$security, $correctness, $maintainability] })
  $raw_count = len({ items: $all_findings })

  # Quarantine malformed entries instead of silently accepting them: `filter`'s `by: "fingerprint"`
  # keeps only items with a truthy `fingerprint` field, so a well-formed finding object survives and
  # a malformed entry (not an object, or missing `fingerprint`) is dropped.
  $well_formed = filter({ items: $all_findings, by: "fingerprint" })
  $well_formed_count = len({ items: $well_formed })

  $unique = dedupe({ items: $well_formed, by: "fingerprint" })
  $ranked = sort({ items: $unique, by: "rank", order: "desc" })
  $finding_count = len({ items: $ranked })

  return { summary: fmt("strict review of {files}: {finding_count} ranked finding(s) from 3 reviewers ({raw_count} raw, {well_formed_count} well-formed)"), findings: $ranked, reviewers: [ "security", "correctness", "maintainability" ], checked_files: $files }
