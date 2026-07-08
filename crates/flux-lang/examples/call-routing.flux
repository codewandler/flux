# call-routing.flux
# Routes an inbound caller utterance to the correct handler.
#
# This is the canonical example of a model-backed op (intent_extract) used as
# a single LLM-cost step — everything else is deterministic routing logic.

type CallerSlots
  destination:  String?
  date:         String?
  passengers:   Number?
  booking_ref:  String?
  reason:       String?

type Intent
  | book_flight
  | change_booking
  | cancel_booking
  | baggage_enquiry
  | escalate_agent

type RouteResult
  intent:    Intent
  slots:     CallerSlots
  response:  String
  escalated: Bool


flow route-call(utterance: String, caller_id: String) -> RouteResult

  # single LLM-cost step: extract intent + slots from the raw utterance
  $extract = intent_extract($utterance,
    schema:  CallerSlots,
    intents: Intent
  )

  # look up any existing booking for this caller (deterministic, no token cost)
  $context = booking_lookup($caller_id)

  # `$extract` is model output — read its fields with the `?` opt-out so an omitted
  # intent/slot falls through to `else` instead of hard-erroring (L-53 makes bare
  # `$x.field` strict; `?` restores the lenient "absent means empty" read).
  $intent = $extract.intent?
  $slots  = $extract.slots?

  when $intent == "book_flight"
    assert $slots.destination?, "no destination found in utterance"
    confirm "Create booking to {slots.destination} on {slots.date}?", risk: medium
      $booking  = booking_create({ slots: $slots, caller: $caller_id })
      $response = fmt("Booking confirmed. Reference {booking.ref}, departing {slots.date}.")
      return {
        intent:    $intent,
        slots:     $slots,
        response:  $response,
        escalated: false
      }

  when $intent == "change_booking"
    assert $context, "no existing booking found for caller"
    $response = booking_modify({ context: $context, slots: $slots })
    return {
      intent:    $intent,
      slots:     $slots,
      response:  $response,
      escalated: false
    }

  when $intent == "cancel_booking"
    assert $context, "no existing booking found for caller"
    confirm "Cancel booking {context.ref} for caller {caller_id}?", risk: high
      booking_cancel($context)
      return {
        intent:    $intent,
        slots:     $slots,
        response:  "Your booking has been cancelled.",
        escalated: false
      }

  # baggage_enquiry, escalate_agent, or anything the model couldn't classify
  else
    $ticket = support_ticket_create({
      caller_id: $caller_id,
      intent:    $intent,
      slots:     $slots,
      utterance: $utterance
    })
    return {
      intent:    $intent,
      slots:     $slots,
      response:  "Connecting you to an agent. Ticket {ticket.id}.",
      escalated: true
    }
