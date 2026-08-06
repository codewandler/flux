# Design — Web version of the Fleet control UI

## Why

Fleet is operated from a terminal TUI on the machine that hosts it. That is the wrong shape for where
Fleet is going: workers are already separate OS processes precisely so they can later run in Docker or
Kubernetes, and a fleet running elsewhere cannot be watched through a local ratatui process at all.

The operator's job is also inherently remote-friendly. Most of it is *reading* — which waves are in
flight, what a worker is doing, which items are parked and why — plus a small set of authority decisions
(accept, accumulate, cancel). None of that needs a terminal; all of it needs to be reachable from
wherever the operator is.

There is a serving substrate to build on rather than invent: `flux docs` serves embedded documentation
and a Flux-Lang workbench, and `flux system` serves the guarded execution transport. The Board and Fleet
control planes are already typed JSON APIs (`--output json` is the documented stable agent API), so the
data layer exists.

## Approach

Serve a read-first web surface over the existing typed control planes, and make every state-changing
action an explicit, audited operation rather than an incidental affordance.

- **Read path first.** Wave and worker status, activity, parked items with their reasons, accepted
  candidates and their tags, worker transcripts. This is the majority of operator value and carries no
  authority risk.
- **Authority path second, and narrow.** Only decisions that are genuinely the operator's: accept a gated
  candidate, trigger accumulation, cancel a wave. Each must be a distinct, confirmable action with a
  recorded receipt — never a side effect of navigation.
- **Reuse the control planes, do not fork them.** The page calls the same Board/Fleet JSON surfaces the
  CLI does. A second implementation of wave-state logic would drift from the authority that enforces it.
- **Loopback vs public is a posture decision, not a default.** `flux docs` already distinguishes loopback
  listeners (which add guarded scratch execution) from public ones (which stay effect-free); this surface
  must make the same distinction explicitly, because it can reach real authority.

Constraints learned from operating the TUI:

- **Sensitive data must not leak into the page.** Worker transcripts, activity logs and Fleet state carry
  absolute host paths, session ids and store locations. Everything rendered must pass through the
  existing redaction path, as `flux export` already does for every rendered string.
- **A worker's evidence is not public by default.** Sibling worktrees and other agents' stores are
  deliberately outside a worker's read scope; this surface must not become the hole that re-exposes them.
- **Bounded rendering.** Fleet state is already multi-megabyte and `events.ndjson` grows without bound, so
  every endpoint needs an explicit limit rather than serving whole files.

## Stories

- Read-only web surface over the Fleet control plane: waves, workers, parked items and their reasons.
- Worker transcript and activity views — redacted, bounded and paged.
- Explicit operator-authority actions (accept, accumulate, cancel) with confirmation and recorded receipts.
- Listener posture decided once at startup: loopback by default, public listeners refuse authority actions.
- Accepted-candidate view: which gated candidates exist, their pinning tags, and what is not yet merged.
