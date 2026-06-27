flow agent-loop -> string
  $answer = fmt("")
  $feedback = fmt("")
  $done = fmt("")
  repeat 25
    until $done
    $plan = plan($feedback)
    $kind = $plan.kind
    match $kind
      case "chat"
        $answer = $plan.text
        $done = fmt("true")
      case "error"
        $answer = $plan.text
        $done = fmt("true")
      default
        $ran = run_plan($plan)
        $feedback = $ran.transcript
        do observe "turn.iteration", $ran
  return $answer
