# advanced-code-review.flux — a production-grade AI code review pipeline, in the REAL
# indentation-based Flux-Lang grammar.
#
# This file used to be written in an aspirational dialect (brace-delimited bodies, a `call("op",
# {...})` meta-op, `@param`, comma-kwarg headers, adjacent string-literal concatenation) that was
# never the language — see docs/stories/L-37-multi-perspective-example.md's grounding precedent,
# which called this file's original style out by name as "aspirational, not parsed". L-41 rewrote
# it against the real grammar (crates/flux-lang/src/parse.rs / docs/syntax.md) so it actually
# parses and lowers.
#
# Demonstrates: parallel, retry/timeout, ctx/ctx_append, fallback, route, the cognition ops
# (ai.extract/ai.judge/ai.rank, synth), confirm (human-in-the-loop escalation), saga+undo with
# once-guarded steps (compensating write + Slack notify), and a final verify — all in native text.
# Every node kind has a native spelling (docs/syntax.md); `@json` remains only as the escape for
# shapes the grammar cannot express, and this file no longer needs it.
#
# `slack.message.send` is an out-of-process `flux-plugin-slack` op: it is not in any in-process
# registry buildable from flux-eval's own dependency set, so `crates/flux-eval/tests/
# examples_validate.rs` pins this file at parse-only (see its `FLOW_PARSE_ONLY`) rather than the
# full `lower` gate every other example gets — a genuine layering boundary, not an oversight.
#
# Run with:
#   flux run examples/advanced-code-review.flux \
#     --param pr_branch=my-feature --param base_branch=main --param notify_channel=#code-review

flow code_review_pipeline(pr_branch: String, base_branch: String, notify_channel: String) -> Answer

  # Phase 1: gather context concurrently. `ci_config` tries two known paths in order; the first
  # that reads successfully wins, else a literal fallback note.
  parallel
    branch $diff_raw
      git_diff()
    branch $readme
      read("README.md")
    branch $ci_config
      fallback
        branch
          read(".github/workflows/ci.yml")
        branch
          read("Taskfile.yaml")
        branch
          "(no CI config found)"

  # Phase 2: guard — abort early if there is nothing to review.
  assert $diff_raw, "No diff found — nothing to review on this branch."

  # Phase 3: a bounded context pack for the audit trail.
  ctx $review_ctx
    purpose "AI code review: diff + project context"
    budget 24000
    include $diff_raw, $readme, $ci_config

  # Phase 4: three model extractions, run concurrently. Security and logic get exponential backoff
  # (2 attempts, 30s each); style gets a single 20s timeout.
  parallel
    branch $security_claims
      retry 2, backoff: exponential
        timeout 30000
          $security_claims = ai.extract({ from: $review_ctx, ask: "Extract every security concern (injection, auth bypass, secret leak, unsafe deserialization) as a Claim with a severity score 1-10.", schema: "Claim[]" })
    branch $logic_claims
      retry 2, backoff: exponential
        timeout 30000
          $logic_claims = ai.extract({ from: $review_ctx, ask: "Extract logic bugs, off-by-one errors, incorrect error handling, and missing edge cases as Claims.", schema: "Claim[]" })
    branch $style_claims
      timeout 20000
        $style_claims = ai.extract({ from: $review_ctx, ask: "Extract style and maintainability issues (naming, duplication, missing docs, dead code) as Claims.", schema: "Claim[]" })

  # Phase 5: merge -> dedupe -> rank -> keep top 10.
  $all_claims = merge({ lists: [$security_claims, $logic_claims, $style_claims] })
  $deduped = dedupe({ items: $all_claims, by: "text" })
  $ranked = ai.rank({ items: $deduped, by: "severity and actionability — security first, then logic bugs, then style" })
  $top_claims = top({ items: $ranked, n: 10 })

  # Phase 6: enrich the context pack and judge overall merge risk.
  $review_ctx += $top_claims

  $verdict = ai.judge({ claim: "This pull request is safe to merge without a blocking security review.", evidence: $review_ctx })

  # Phase 7: model-routed action. `ai.reason` picks exactly one label; the runtime runs the
  # matching fixed branch — the model chooses which branch runs, never what it does.
  route ai.reason({ ask: "Given the verdict, output exactly one word: 'approve', 'request_changes', or 'escalate'.", ctx: $review_ctx })
    case "approve"
      $action_label = "APPROVED"
    case "request_changes"
      $action_label = "CHANGES_REQUESTED"
    case "escalate"
      $action_label = "ESCALATED"
      # `confirm` — human-in-the-loop gate; the body runs only on approval, denial errors the arm.
      confirm "AI flagged this PR for security escalation. Review the top claims and approve sending the escalation notice.", risk: high
        observe({ kind: "escalation.approved" })
    default
      $action_label = "CHANGES_REQUESTED"

  # Phase 8: synthesize a cited markdown report.
  $report = synth({ claims: $top_claims, format: "markdown", cite: true })

  # Phase 9: write + notify — a saga with compensating rollback. If the Slack step fails, the saga
  # unwinds and the undo for step 1 deletes the file that was just written. Each step is guarded by
  # `once`, so a replayed flow never double-writes the report or double-posts to Slack. The
  # interpolated path/content/command/text are pre-bound to symbols with `fmt` (a call argument
  # takes values — `lit`/`var`/obj/list — never a bare `fmt`; bind first, then pass `$name`,
  # exactly like every other call in this flow).
  $report_path = fmt(".flux/reviews/{pr_branch}.md")
  $report_content = fmt("# Code Review: {pr_branch}\n\n**Decision**: {action_label}\n\n{report}")
  $rm_cmd = fmt("rm -f .flux/reviews/{pr_branch}.md")
  $slack_text = fmt(":mag: *Code Review Complete* — `{pr_branch}` -> *{action_label}*\n{report}")
  saga
    step
      once "write-review-report"
        write({ path: $report_path, content: $report_content })
    undo
      bash($rm_cmd)
    step
      budget 3
        once "post-slack-review"
          slack.message.send({ channel: $notify_channel, text: $slack_text })

  # Phase 10: verify the report file actually landed.
  verify path_exists($report_path) contains "true": "Review file was not written - saga compensation may have run."

  return { status: $action_label, summary: $report, evidence: $top_claims, gaps: [], risks: $verdict }
