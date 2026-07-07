---
id: A-57
title: A2A push notifications — pushNotificationConfig methods + webhook delivery
pillar: Agent
status: done
epic: a2a-conformance
design: docs/designs/a2a-stateful-task-model.md
note: "Tier-3, last/optional slice: per-task webhook config + delivery; flips capabilities.pushNotifications"
---

# A2A push notifications

## Goal
Let a client register a webhook for a task and have flux POST the task's status/artifact updates to
that URL as a background run advances — implementing `tasks/pushNotificationConfig/{set,get,list,delete}`
and a delivery layer, and flipping `capabilities.pushNotifications` to `true`.

## Why (evidence)
- The four `pushNotificationConfig/*` methods have no server code (`-32004`, A-50);
  `capabilities.pushNotifications` is advertised `false` (honest).
- The delivery payloads already exist: `TaskStatusUpdateEvent` / `TaskArtifactUpdateEvent` shapers
  (A-52). Presupposes the non-blocking execution + task projection from A-54.

## Acceptance
- [ ] `tasks/pushNotificationConfig/{set,get,list,delete}` store/read/remove a per-task webhook config,
      realm-scoped; unknown/other-realm task id → `-32001`.
- [ ] A delivery layer POSTs `TaskStatusUpdateEvent` / `TaskArtifactUpdateEvent` frames to the
      registered URL as the background run advances (best-effort, with a documented retry/failure
      policy).
- [ ] `capabilities.pushNotifications` is emitted `true` once enabled; `-32003
      PushNotificationNotSupported` becomes the "configured off / not enabled" answer.
- [ ] Failing-first tests: a `set` then a task run delivers ≥1 update to a test webhook; `get`/`list`
      reflect the config; `delete` stops delivery; the card capability flips.

## Progress
- 2026-07-08 — done. `tasks/pushNotificationConfig/{set,get,list,delete}` in
  `crates/flux-server/src/a2a.rs` (`push_config`), realm-scoped (the task must resolve within the
  caller's realm before the config surface is touched; unknown/cross-realm → `-32001`). Configs
  live in-process beside the live-task map (delivery only happens for in-process runs, so
  durability beyond the process buys nothing — documented: re-register after restart); config id
  defaults to its URL. Delivery = `deliver_push` fanned out from `publish_transition`: one POST
  per status **transition** (never per-token deltas), fire-and-forget on a spawned task, 10s
  timeout, failures logged, **no retry** (documented policy — the durable projection is the
  source of truth; push is a hint to poll); config `token` rides as `X-A2A-Notification-Token`.
  SSRF posture (`push_url_allowed`, documented): only public `http(s)` — loopback/private/
  link-local/unspecified literals and `localhost` refused with `-32003
  PushNotificationNotSupported`; DNS-rebinding out of scope (network-layer concern);
  `FLUX_A2A_PUSH_ALLOW_LOCAL=1` for local dev/tests. `capabilities.pushNotifications` flipped to
  `true` (new `AgentCard::with_push_notifications`). Test:
  `push_notification_config_and_delivery` (set/list/refused-URL/delivery-to-live-webhook/delete
  stops delivery; card flip).

## Notes
- Shipped with A-54..A-56 in one Tier-3 pass. Epic:
  [a2a-conformance](../designs/a2a-conformance.md).
