-- advanced-code-review.flux --
--
-- A production-grade AI code review pipeline.
-- Demonstrates: parallel, saga+undo, route, retry/timeout, ctx/ctx_append,
--   once (idempotency), budget, fallback, verify, ai.extract, ai.judge,
--   ai.rank, synth, and a final Slack notification gate.
--
-- Run with:
--   flux run examples/advanced-code-review.flux \
--     --param pr_branch=my-feature \
--     --param base_branch=main \
--     --param notify_channel=#code-review

flow code_review_pipeline(
  $pr_branch:      string,
  $base_branch:    string,
  $notify_channel: string
) -> Answer {

  -- ── Phase 1: gather context concurrently ────────────────────────────────
  -- Three independent reads fan out in parallel; none depends on another.
  -- ci_config tries three paths in order; first non-empty wins (fallback).
  parallel {
    diff_raw: {
      git_diff()
    }
    readme: {
      read("README.md")
    }
    ci_config: {
      fallback {
        { read(".github/workflows/ci.yml") }
        { read("Taskfile.yaml") }
        { "(no CI config found)" }
      }
    }
  }

  -- ── Phase 2: guard — abort early if there is nothing to review ─────────
  assert len($diff_raw) > 0
    message "No diff found — nothing to review on this branch."

  -- ── Phase 3: build a bounded context pack ──────────────────────────────
  -- budget 24000 caps the pack at 24 000 chars; the runtime drops the
  -- lowest-priority members if that threshold is exceeded and records
  -- what was dropped in the run trace.
  ctx review_ctx
    include [diff_raw, readme, ci_config]
    budget  24000
    purpose "AI code review: diff + project context"

  -- ── Phase 4: three model extractions — run concurrently ────────────────
  -- Security and logic extractors get exponential backoff (2 retries, 30s);
  -- style gets a single 20s timeout. All three run concurrently.
  parallel {
    security_claims: {
      retry max 2 backoff exponential {
        timeout 30000 {
          ai.extract(
            from:   $review_ctx,
            ask:    "Extract every security concern (injection, auth bypass, "
                    "secret leak, unsafe deserialization, …) as a Claim "
                    "with a severity score 1–10.",
            schema: "Claim[]"
          )
        }
      }
    }
    logic_claims: {
      retry max 2 backoff exponential {
        timeout 30000 {
          ai.extract(
            from:   $review_ctx,
            ask:    "Extract logic bugs, off-by-one errors, incorrect error "
                    "handling, and missing edge cases as Claims.",
            schema: "Claim[]"
          )
        }
      }
    }
    style_claims: {
      timeout 20000 {
        ai.extract(
          from:   $review_ctx,
          ask:    "Extract style and maintainability issues (naming, "
                  "duplication, missing docs, dead code) as Claims.",
          schema: "Claim[]"
        )
      }
    }
  }

  -- ── Phase 5: merge → dedupe → rank → keep top 10 ───────────────────────
  $all_claims   = merge([$security_claims, $logic_claims, $style_claims])
  $deduped      = dedupe($all_claims, by: "text")
  $ranked       = ai.rank(
                    $deduped,
                    by: "severity and actionability — security first, "
                        "then logic bugs, then style"
                  )
  $top_claims   = top($ranked, n: 10)

  -- ── Phase 6: enrich context and judge overall merge risk ────────────────
  ctx_append review_ctx += [top_claims]

  $verdict = ai.judge(
    claim:    "This pull request is safe to merge without a blocking security review.",
    evidence: $review_ctx
  )

  -- ── Phase 7: model-routed action ────────────────────────────────────────
  -- ai.reason produces exactly one label; the runtime picks the matching
  -- fixed branch. The model chooses *which* branch runs, never *what* it does.
  route ai.reason(
    ask: "Given the verdict, output exactly one word: "
         "'approve', 'request_changes', or 'escalate'.",
    ctx: $review_ctx
  ) {
    approve         => { $action_label = "APPROVED"           }
    request_changes => { $action_label = "CHANGES_REQUESTED"  }
    escalate        => {
      $action_label = "ESCALATED"
      -- Human gate: must approve before the escalation notice goes out.
      confirm risk high
        "AI flagged this PR for security escalation. "
        "Review the top claims and approve sending the escalation notice." {
        observe(kind: "escalation.approved", data: {branch: $pr_branch})
      }
    }
  }
  default { $action_label = "CHANGES_REQUESTED" }

  -- ── Phase 8: synthesize a cited markdown report ─────────────────────────
  $report = synth(
    claims: $top_claims,
    format: "markdown",
    cite:   true
  )

  -- ── Phase 9: write + notify — saga with compensating rollback ───────────
  -- If the Slack step fails, the saga unwinds and the undo for step 1
  -- deletes the file that was just written.
  saga {
    step {
      -- once: idempotent across re-runs — the file is written at most once.
      once "write-review-report-{pr_branch}" bind report_path {
        write(
          path:    ".flux/reviews/{pr_branch}.md",
          content: "# Code Review: {pr_branch}\n\n"
                   "**Decision**: {action_label}\n\n"
                   "{report}"
        )
      }
    }
    undo {
      -- Compensate: remove the file if a later step fails.
      bash("rm -f .flux/reviews/{pr_branch}.md")
    }

    step {
      -- budget: cap the Slack notification at 3 op dispatches.
      budget 3 {
        once "post-slack-review-{pr_branch}" {
          slack.message.send(
            channel: $notify_channel,
            text:    ":mag: *Code Review Complete* — "
                     "`{pr_branch}` → *{action_label}*\n{report}"
          )
        }
      }
    }
    undo {
      -- Slack messages cannot be unsent; compensation is a no-op here.
    }
  }

  -- ── Phase 10: verify the report file actually landed ───────────────────
  -- If saga compensation ran (Slack failed → file deleted), this surfaces
  -- it as a clear error rather than silently returning success.
  verify path_exists(".flux/reviews/{pr_branch}.md") contains "true"
    message "Review file was not written — saga compensation may have run."

  -- ── Return a structured Answer ──────────────────────────────────────────
  return Answer {
    status:   $action_label,
    summary:  $report,
    evidence: $top_claims,
    gaps:     [],
    risks:    $verdict
  }
}
