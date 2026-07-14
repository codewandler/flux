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
    case "decision"
      # Deterministic routing found more than one live integration. The common decision case below
      # presents and parks it before any integration schema is exposed.
      $step = $intent
    default
      # Exploration uses provider-native schemas. Safe reads run through the envelope immediately;
      # effectful calls are captured into a host-built ActionBatch.
      $step = explore({ state: $intent.state })

  # Explore/decide/execute/revise is bounded. Every decision uses the same durable await, including
  # decisions discovered after an execution report. Each batch gets a separate aggregate approval
  # and a one-shot receipt; reports return to the same native ledger, so completed work is not rerun.
  repeat 50
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
        $answer = present_results({ step: $step })
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

    # `await` is a direct child of this bounded repeat. The built-in loop compiler lifts each copy
    # to a top-level durable cursor while preserving the repeat's finite `until $done` state machine.
    $question = $step.question?
    # A batch may produce a decision in the same iteration, after the match above already selected
    # its `batch` arm. Render that newly discovered question before parking as well.
    when $question
      $answer = present_results({ step: $step })
    # Keep the authored prompt as the suspension's last emitted value. Without this bind, the
    # lifted top-level cursor would surface the raw question object on decisions after round one.
    $suspension_prompt = $answer
    await $decision = "agent.decision" when $question
    when $question
      $step = explore({ state: $step.state, decision: $decision })
  return $answer
