# demo.flux — a showcase for the Flux-Lang plugin.
# Open this in the sandbox IDE to see highlighting, folding, completion, and validation.

type CallerSlots
  destination: String
  date:        String?
  passengers:  Number

type Intent
  | book_flight
  | change_booking
  | escalate_agent

flow route-call(utterance: String, caller_id: String) -> RouteResult
  # one model-cost step: extract intent + slots
  $extract = intent_extract($utterance, schema: CallerSlots, intents: Intent)
  $context = booking_lookup($caller_id)

  @effect(network)
  $page = web_fetch("https://status.example.com")

  when $extract.intent == "book_flight"
    $slots = $extract.slots
    assert $slots.destination, "no destination found"
    confirm "Create booking to {slots.destination}?", risk: medium
      $booking = booking_create($slots, caller: $caller_id)
      return { intent: $extract.intent, escalated: false }
  else
    repeat 3
      until $done
      $done = poll("queue", limit: 50)
    return "escalated"
