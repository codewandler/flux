# zendesk.triage.flux — deterministic Zendesk automation with bounded AI analysis.
#
# ─────────────────────────────────────────────────────────────────────────────────────────────────
# WHO SERVES THESE OPERATIONS — the flux-connectors Tool pack, and what it still cannot do.
#
# The first-party `zendesk` plugin this flow was written against was REMOVED before it ever shipped.
# Its replacement is flux-connectors' `connector-pack`: a host registers it with
# `ClientBuilder::try_register_pack(connector_pack::pack(&["zendesk"], http, credentials))` and gets
# one dotted tool per catalogue operation, each delegating to flux's OWN `http.request` — so flux
# keeps every byte of egress, and each operation is gated individually by this host's permission and
# approval envelope rather than inheriting whatever `http.request` happens to get.
#
# The op names below did NOT have to change: the pack projects `zendesk-test` to `zendesk.test` and
# `zendesk-ticket-comment-list` to `zendesk.ticket.comment.list`, which is exactly what this flow
# already called. The pack was authored to this shape. Two guards keep it that way — the exact
# operation set is pinned here by `flux-cli`'s `zendesk_reference_calls_exactly_the_connector_pack_
# read_operations`, and by the pack's own `projection.rs` at the other end.
#
# STILL NOT RUNNABLE AGAINST A LIVE ACCOUNT — but for ONE connector-side reason, not two, and it is
# not a missing credential:
#
#   `providers/zendesk.toml` declares no `authority`, so the pack has no address to look a credential
#   up at and refuses with `NoCredentialAddress` rather than sending an unauthenticated request. Only
#   7 of the 44 shipped connectors declare one. Owned by flux-connectors C-92 (`ready`), under
#   C-37's addressing scheme. Storing a token does not work around it: the address is what is
#   missing, not the value.
#
# A SECOND blocker was recorded here and is now CLOSED (2026-07-31). Zendesk's `base_url` is
# `https://{subdomain}.zendesk.com`, and the pack used to emit that placeholder verbatim. It now
# takes a `Configuration` port as a fourth argument to `pack()` and substitutes template variables
# **totally or refuses** — one unbound variable is `MissingConfig`, naming it. A built URL no longer
# carries a placeholder.
#
# So every entrypoint still fails at its first call, but it fails in one named, fixable place rather
# than at an operation nothing serves.
#
# ⚠ Counts and blockers here are point-in-time and have already moved twice. Re-derive from
# flux-connectors' `providers/*.toml` and its board rather than trusting this comment.
#
# The flow structure is unchanged and is the point: four read-only entrypoints, retry with
# exponential backoff, bounded contexts with explicit budgets, model timeouts, and a deterministic
# evidence fallback. Its coverage is provider-free — the tests drive these entrypoints against
# stubbed operations, so the shape stays enforced without credentials or network.
# ─────────────────────────────────────────────────────────────────────────────────────────────────
#
# Examples (each needs the two connector-side gaps above closed before it can reach Zendesk):
#   flux run examples/zendesk.triage.flux --entry setup --yes
#   flux run examples/zendesk.triage.flux --entry triage --arg 'query=type:ticket status:new' --yes
#   flux run examples/zendesk.triage.flux --entry brief --arg ticket_id=12345 --yes
#   flux run examples/zendesk.triage.flux --entry eod --arg 'query=type:ticket updated>24hours' --yes
#
# This reference is intentionally READ-ONLY: no write operation is reachable from any of these four
# flows. Note what that does and does not mean now that the pack serves them — `pack(&["zendesk"])`
# registers all seven catalogue operations, so `zendesk.ticket.update`, `zendesk.ticket.comment.add`
# and `zendesk.ticket.tag.add` are present in the host's registry. What is guaranteed is that no
# entrypoint HERE reaches one, asserted from this module's own call graph rather than by comment; a
# host that wants them unreachable altogether withholds approval, which is where that decision
# belongs. Ticket descriptions and
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
