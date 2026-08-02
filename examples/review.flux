# review.flux — classify a project, derive project-specific review dimensions, fan out, and judge.
#
# Run from this checkout (the checked-in reviewer roles are resolved from `.flux/agents`):
#   flux flow run examples/review.flux -m <model> --yes
#
# The two cognition stages deliberately have different inputs. `classifications` sees the complete
# guarded workspace inventory plus recent Git history. `dimensions` sees ONLY `classification` —
# never `files`, `history`, or `project_context`. The authored four-way parallel block is the spend
# and concurrency ceiling; the model selects review content, not how many agents may be launched.

flow review -> String
  parallel
    branch $files
      $files = glob({ pattern: "*" })
    branch $history
      $history = git_log({ limit: 50 })

  ctx $project_context
    purpose "classify this project from its non-trash directory structure and recent Git history"
    include $files, $history

  $classifications = ai.extract({ from: $project_context, ask: "Classify this software project. Return exactly one classification object. Describe what it is, its primary languages and frameworks, architecture, delivery surfaces, maturity signals, and the risks implied by its recent development history. Stay descriptive: do not propose review dimensions or findings yet.", schema: "{project_kind: String, purpose: String, languages: [String], frameworks: [String], architecture: [String], delivery_surfaces: [String], maturity_signals: [String], history_signals: [String], risk_profile: [String]}" })
  $classification = $classifications.0

  # This call's only variable input is `$classification`; L-129's structural test pins that seam.
  $dimensions = ai.extract({ from: $classification, ask: "Using only this project classification, derive exactly four distinct, high-value review dimensions appropriate for this kind of project. Cover the most consequential concerns without defaulting to a generic fixed checklist. Each dimension must be independently reviewable by one agent.", schema: "{name: String, priority: String, rationale: String, questions: [String]}" })

  $dimension_1 = $dimensions.0
  $dimension_2 = $dimensions.1
  $dimension_3 = $dimensions.2
  $dimension_4 = $dimensions.3
  $dimension_5 = $dimensions.4?
  assert $dimension_4, "dimension derivation must return four usable review dimensions"
  assert !$dimension_5, "dimension derivation must return exactly four review dimensions"

  $prompt_1 = fmt("Review the current project through this assigned dimension: {dimension_1}\n\nProject classification: {classification}\n\nInspect the repository yourself with read-only tools. Report only evidence-backed findings with file and line locations where possible.")
  $prompt_2 = fmt("Review the current project through this assigned dimension: {dimension_2}\n\nProject classification: {classification}\n\nInspect the repository yourself with read-only tools. Report only evidence-backed findings with file and line locations where possible.")
  $prompt_3 = fmt("Review the current project through this assigned dimension: {dimension_3}\n\nProject classification: {classification}\n\nInspect the repository yourself with read-only tools. Report only evidence-backed findings with file and line locations where possible.")
  $prompt_4 = fmt("Review the current project through this assigned dimension: {dimension_4}\n\nProject classification: {classification}\n\nInspect the repository yourself with read-only tools. Report only evidence-backed findings with file and line locations where possible.")

  parallel
    branch $review_1
      $review_1 = task({ role: "review-project", task: $prompt_1 })
    branch $review_2
      $review_2 = task({ role: "review-project", task: $prompt_2 })
    branch $review_3
      $review_3 = task({ role: "review-project", task: $prompt_3 })
    branch $review_4
      $review_4 = task({ role: "review-project", task: $prompt_4 })

  $joined_reports = join({ items: [$review_1, $review_2, $review_3, $review_4], sep: "\n\n--- NEXT REVIEW DIMENSION ---\n\n" })
  $verdict_prompt = fmt("""
Produce one final project-review verdict from the classification, the four selected dimensions, and
the four reviewer reports below.

Rules:
- de-duplicate overlapping findings and reject claims that lack repository evidence;
- preserve file and line locations and identify which dimension(s) support each finding;
- rank actionable findings by severity, then confidence;
- distinguish confirmed findings from gaps that need more evidence;
- end with an explicit overall verdict and the three highest-value next actions.

## Project classification
{classification}

## Selected dimensions
{dimensions}

## Joined reviewer reports
{joined_reports}
""")
  $verdict = task({ role: "review-synthesizer", task: $verdict_prompt })
  return $verdict
