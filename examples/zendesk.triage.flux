# zendesk.triage.flux — deterministic Zendesk automation with bounded AI analysis.
#
# ─────────────────────────────────────────────────────────────────────────────────────────────────
# NOT RUNNABLE AS SHIPPED — the backing integration is being replaced.
#
# The first-party `zendesk` plugin this flow was written against has been REMOVED. It is to be
# superseded shortly by a flux-connectors interop layer, and the `zendesk.*` operations below will be
# re-pointed at that layer once it lands. Until then no provider serves them, so every entrypoint
# here fails at its first call.
#
# This file is kept deliberately, as the authored shape the replacement has to satisfy: four
# read-only entrypoints, retry with exponential backoff, bounded contexts with explicit budgets,
# model timeouts, and a deterministic evidence fallback. The op NAMES are the part expected to
# change; the flow structure is not. Its coverage is provider-free — the tests drive these
# entrypoints against stubbed operations, so they keep passing and keep this shape honest while the
# integration is absent.
# ─────────────────────────────────────────────────────────────────────────────────────────────────
#
# Examples (each awaits the replacement integration before it can run):
#   flux run examples/zendesk.triage.flux --entry setup --yes
#   flux run examples/zendesk.triage.flux --entry triage --arg 'query=type:ticket status:new' --yes
#   flux run examples/zendesk.triage.flux --entry brief --arg ticket_id=12345 --yes
#   flux run examples/zendesk.triage.flux --entry eod --arg 'query=type:ticket updated>24hours' --yes
#
# This reference is intentionally READ-ONLY: no write operation is reachable from any of these four
# flows, and the replacement integration is expected to preserve that — a write must stay separately
# approval-gated and concurrency-safe rather than becoming reachable here. Ticket descriptions and
# internal comments enter the configured model context in triage/brief/eod. Every model call is
# bounded by a timeout and fallback; a provider failure returns the gathered Zendesk evidence.

flow setup() -> Any
  retry 3, backoff: exponential, delay: 500ms -> $identity
    zendesk.test({})
  return $identity

flow triage(query: String) -> Any
  retry 3, backoff: exponential, delay: 500ms -> $search
    zendesk.ticket.search({ query: $query, page: 1, per_page: 100 })

  ctx $triage_context
    purpose "Prioritize one bounded page of Zendesk tickets; cite every recommendation by ticket id"
    budget 18000
    include $search

  fallback -> $analysis
    branch
      timeout 30000 -> $model_analysis
        ai.extract({ from: $triage_context, ask: "Prioritize these tickets by urgency and customer impact. For every item return ticket_id, priority, rationale, and next_action. Do not propose or perform writes.", schema: "[{ticket_id: Number, priority: String, rationale: String, next_action: String}]" })
    branch
      fmt("AI analysis unavailable; raw bounded Zendesk evidence follows: {search}")

  return { query: $query, evidence: $search, analysis: $analysis }

flow brief(ticket_id: Number) -> Any
  parallel
    branch $ticket
      retry 3, backoff: exponential, delay: 500ms
        zendesk.ticket.show({ ticket_id: $ticket_id })
    branch $comments
      retry 3, backoff: exponential, delay: 500ms
        zendesk.ticket.comment.list({ ticket_id: $ticket_id, page: 1, per_page: 100 })

  ctx $brief_context
    purpose "Build a factual support brief from one ticket and one bounded page of comments"
    budget 24000
    include $ticket, $comments

  fallback -> $briefing
    branch
      timeout 30000 -> $model_brief
        ai.extract({ from: $brief_context, ask: "Return a factual timeline, current customer need, open questions, risks, and the next recommended action. Cite the ticket and comment ids. Do not propose or perform writes.", schema: "{timeline: [String], customer_need: String, open_questions: [String], risks: [String], next_action: String}" })
    branch
      fmt("AI briefing unavailable; ticket evidence: {ticket}; comment evidence: {comments}")

  return { ticket_id: $ticket_id, ticket: $ticket, comments: $comments, briefing: $briefing }

flow eod(query: String) -> Any
  retry 3, backoff: exponential, delay: 500ms -> $search
    zendesk.ticket.search({ query: $query, page: 1, per_page: 100 })

  ctx $eod_context
    purpose "Summarize the end-of-day state of one bounded Zendesk ticket result page"
    budget 18000
    include $search

  fallback -> $summary
    branch
      timeout 30000 -> $model_summary
        ai.extract({ from: $eod_context, ask: "Summarize completed work, urgent carry-over, blocked tickets, and tomorrow's first actions. Cite ticket ids and do not propose or perform writes.", schema: "{completed: [String], urgent_carry_over: [String], blocked: [String], tomorrow: [String]}" })
    branch
      fmt("AI summary unavailable; raw bounded Zendesk evidence follows: {search}")

  return { query: $query, evidence: $search, summary: $summary }
