flow agent-loop -> string
  $answer = fmt("")
  $feedback = fmt("")
  $done = fmt("")

  # Pass 1 -- orient: one planner call, a three-way contract -- trivial request -> prose chat;
  # simple/actionable request -> the full execution plan; complex/context-hungry request -> a
  # small read-only gather plan + brief. $settled is "" only for the gather case.
  $plan = plan({ feedback: $feedback, phase: "orient" })
  $settled = $plan.settled

  # Pass 2 -- gather: bounded, read-only, approval-free rounds while not yet settled. Skipped
  # entirely when orient already settled, so a trivial/simple turn adds zero latency here.
  unless $settled
    repeat 3
      until $settled
      $ran = run_plan($plan)
      $feedback = $ran.transcript
      do observe "turn.gather", $ran
      $plan = plan({ feedback: $feedback, phase: "gather" })
      $settled = $plan.settled

  # Pass 3 -- plan / execute / revise: the standard loop, unchanged guards. A leftover gather
  # plan (the budget exhausted before settling) simply runs as the first execute iteration.
  repeat 25
    until $done
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
        # A-17 -- revise wiring: $ran.failure is a reified halt (design Part 2) when this round's
        # plan failed mid-way -- absent (host-normalized to null) on a clean run. Route the
        # observation on it so a revision round is told apart from a plain iteration; the
        # feedback text itself (kind-specific guidance, the fatal/retryable distinction, the
        # denial re-emission guard) is already built by the host into $ran.transcript above.
        $failure = $ran.failure
        when $failure
          do observe "turn.revision", $ran
        else
          do observe "turn.iteration", $ran
        $plan = plan({ feedback: $feedback, phase: "execute" })
  return $answer
