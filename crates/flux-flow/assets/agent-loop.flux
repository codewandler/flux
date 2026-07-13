flow agent-loop -> string
  $answer = fmt("")
  $done = fmt("")

  # The model declares intent; the host intersects its signals with the live, wired, permitted
  # registry. This stage cannot call a leaf operation or grant authority.
  $intent = detect_intent()
  $step = $intent
  $intent_kind = $intent.kind
  match $intent_kind
    case "error"
      $answer = present_results({ step: $intent })
      $done = fmt("true")
    default
      # Exploration uses provider-native schemas. Safe reads run through the envelope immediately;
      # effectful calls are captured into a host-built ActionBatch.
      $step = explore({ state: $intent.state })

  # A decision discovered during exploration is a real durable suspension point in this authored
  # flow. A false condition skips it; on resume the existing flow store restores every prior bind,
  # including the opaque native conversation ledger inside $step.state.
  $question = $step.question?
  when $question
    $answer = present_results({ step: $step })
  await $decision = "agent.decision" when $question
  when $question
    $step = explore({ state: $step.state, decision: $decision })

  # Execute/revise is bounded. Each batch gets a separate aggregate approval and a one-shot receipt;
  # execution reports return to the same native ledger, so only failed work is repaired.
  repeat 25
    until $done
    $kind = $step.kind
    match $kind
      case "chat"
        $answer = present_results({ step: $step })
        $done = fmt("true")
      case "error"
        $answer = present_results({ step: $step })
        $done = fmt("true")
      case "decision"
        # A second decision after an execution report is surfaced honestly. The next refinement is
        # a repeatable top-level decision sub-flow; never guess or execute past it.
        $answer = present_results({ step: $step })
        $done = fmt("true")
      case "batch"
        $receipt = approve_batch({ batch: $step.batch })
        $approved = $receipt.approved
        when $approved
          $report = execute_batch({ batch: $step.batch, receipt: $receipt })
          observe({ kind: "turn.execution", data: $report })
          $step = explore({ state: $step.state, report: $report })
        else
          $answer = present_results({ approval: $receipt })
          $done = fmt("true")
      default
        $answer = fmt("The adaptive loop returned an unknown stage result and stopped safely.")
        $done = fmt("true")
  return $answer
